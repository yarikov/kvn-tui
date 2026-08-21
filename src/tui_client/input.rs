//! Terminal input with layout-independent shortcut support.
//!
//! Crossterm 0.29 can request Kitty keyboard enhancements, but it discards the
//! protocol's third (US base-layout) key code. That code is exactly what a
//! command-oriented TUI needs: the physical key that is `j` on a PC-101
//! layout remains `j` while the active layout produces Cyrillic, Arabic, or
//! another script. This small reader keeps using Crossterm's public event
//! types while decoding the subset of terminal input used by kvn-tui.

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventState, KeyModifiers};
use signal_hook::consts::signal::SIGWINCH;
use signal_hook::iterator::Signals;

use crate::app::msg::Msg;

/// Push Kitty keyboard flags in the alternate screen:
/// disambiguate escape codes, report alternate keys, and encode all keys.
pub(super) const PUSH_KEYBOARD_PROTOCOL: &str = "\x1b[>13u";
/// Restore the keyboard flags that were active before kvn-tui started.
pub(super) const POP_KEYBOARD_PROTOCOL: &str = "\x1b[<1u";

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const ESCAPE_TIMEOUT: Duration = Duration::from_millis(30);

pub(super) fn enable_keyboard_protocol(out: &mut impl Write) -> io::Result<()> {
    out.write_all(PUSH_KEYBOARD_PROTOCOL.as_bytes())?;
    out.flush()
}

pub(super) fn disable_keyboard_protocol(out: &mut impl Write) -> io::Result<()> {
    out.write_all(POP_KEYBOARD_PROTOCOL.as_bytes())?;
    out.flush()
}

pub(super) fn spawn_event_reader(tx: Sender<Msg>, reading_enabled: Arc<AtomicBool>) {
    spawn_resize_reader(tx.clone());
    thread::spawn(move || {
        let mut decoder = Decoder::default();
        let mut stdin = io::stdin().lock();
        let mut bytes = [0_u8; 64];

        loop {
            if !reading_enabled.load(Ordering::Relaxed) {
                thread::sleep(POLL_INTERVAL);
                continue;
            }

            match stdin_ready(POLL_INTERVAL) {
                Ok(true) => match stdin.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(read) => decoder.push(&bytes[..read]),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                },
                Ok(false) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }

            for key in decoder.drain(Instant::now()) {
                if tx.send(Msg::Key(key)).is_err() {
                    return;
                }
            }
        }
    });
}

fn spawn_resize_reader(tx: Sender<Msg>) {
    thread::spawn(move || {
        let Ok(mut signals) = Signals::new([SIGWINCH]) else {
            return;
        };
        for _ in signals.forever() {
            if tx.send(Msg::Resize).is_err() {
                break;
            }
        }
    });
}

/// Drop keystrokes queued while the external editor owned the terminal.
#[allow(unsafe_code)]
pub(super) fn discard_pending_input() {
    let mut bytes = [0_u8; 64];
    while matches!(stdin_ready(Duration::ZERO), Ok(true)) {
        // SAFETY: `bytes` is a valid writable allocation and STDIN_FILENO is
        // borrowed, not closed or retained. A nonblocking readiness check is
        // performed immediately before every read.
        let read =
            unsafe { libc::read(libc::STDIN_FILENO, bytes.as_mut_ptr().cast(), bytes.len()) };
        if read <= 0 {
            break;
        }
    }
}

#[allow(unsafe_code)]
fn stdin_ready(timeout: Duration) -> io::Result<bool> {
    let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
    let mut fd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `fd` points to one initialized pollfd for the duration of the
    // call. poll neither owns nor outlives the referenced file descriptor.
    let result = unsafe { libc::poll(&mut fd, 1, millis) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(result > 0 && fd.revents & libc::POLLIN != 0)
}

#[derive(Default)]
struct Decoder {
    pending: Vec<u8>,
    escape_started: Option<Instant>,
}

impl Decoder {
    fn push(&mut self, bytes: &[u8]) {
        if self.pending.is_empty() && bytes.first() == Some(&0x1b) {
            self.escape_started = Some(Instant::now());
        }
        self.pending.extend_from_slice(bytes);
    }

    fn drain(&mut self, now: Instant) -> Vec<KeyEvent> {
        let mut events = Vec::new();
        loop {
            match parse_one(&self.pending, self.escape_expired(now)) {
                ParseResult::Event(key, used) => {
                    self.pending.drain(..used);
                    self.reset_escape_timer();
                    events.push(key);
                }
                ParseResult::Discard(used) => {
                    self.pending.drain(..used);
                    self.reset_escape_timer();
                }
                ParseResult::Incomplete => break,
            }
        }
        events
    }

    fn escape_expired(&self, now: Instant) -> bool {
        self.escape_started
            .is_some_and(|started| now.saturating_duration_since(started) >= ESCAPE_TIMEOUT)
    }

    fn reset_escape_timer(&mut self) {
        self.escape_started = (self.pending.first() == Some(&0x1b)).then(Instant::now);
    }
}

enum ParseResult {
    Event(KeyEvent, usize),
    Discard(usize),
    Incomplete,
}

fn parse_one(bytes: &[u8], escape_expired: bool) -> ParseResult {
    let Some(&first) = bytes.first() else {
        return ParseResult::Incomplete;
    };
    match first {
        0x1b => parse_escape(bytes, escape_expired),
        b'\r' | b'\n' => event(KeyCode::Enter, KeyModifiers::NONE, 1),
        b'\t' => event(KeyCode::Tab, KeyModifiers::NONE, 1),
        0x7f => event(KeyCode::Backspace, KeyModifiers::NONE, 1),
        0x01..=0x1a => event(
            KeyCode::Char((b'a' + first - 1) as char),
            KeyModifiers::CONTROL,
            1,
        ),
        _ => parse_utf8(bytes, KeyModifiers::NONE, 0),
    }
}

fn parse_escape(bytes: &[u8], expired: bool) -> ParseResult {
    if bytes.len() == 1 {
        return if expired {
            event(KeyCode::Esc, KeyModifiers::NONE, 1)
        } else {
            ParseResult::Incomplete
        };
    }
    match bytes[1] {
        b'[' => parse_csi(bytes),
        b'O' => parse_ss3(bytes),
        _ => parse_utf8(&bytes[1..], KeyModifiers::ALT, 1),
    }
}

fn parse_ss3(bytes: &[u8]) -> ParseResult {
    if bytes.len() < 3 {
        return ParseResult::Incomplete;
    }
    let code = match bytes[2] {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        _ => return ParseResult::Discard(3),
    };
    event(code, KeyModifiers::NONE, 3)
}

fn parse_csi(bytes: &[u8]) -> ParseResult {
    let Some(relative_end) = bytes[2..]
        .iter()
        .position(|byte| (0x40..=0x7e).contains(byte))
    else {
        return ParseResult::Incomplete;
    };
    let end = relative_end + 2;
    let used = end + 1;
    let Ok(parameters) = std::str::from_utf8(&bytes[2..end]) else {
        return ParseResult::Discard(used);
    };
    match bytes[end] {
        b'u' => parse_csi_u(parameters, used),
        b'A' => modified_key(KeyCode::Up, parameters, used),
        b'B' => modified_key(KeyCode::Down, parameters, used),
        b'C' => modified_key(KeyCode::Right, parameters, used),
        b'D' => modified_key(KeyCode::Left, parameters, used),
        b'H' => modified_key(KeyCode::Home, parameters, used),
        b'F' => modified_key(KeyCode::End, parameters, used),
        b'Z' => event(KeyCode::BackTab, KeyModifiers::SHIFT, used),
        b'~' => match parameters.split(';').next().unwrap_or_default() {
            "2" => event(KeyCode::Insert, KeyModifiers::NONE, used),
            "3" => event(KeyCode::Delete, KeyModifiers::NONE, used),
            "5" => event(KeyCode::PageUp, KeyModifiers::NONE, used),
            "6" => event(KeyCode::PageDown, KeyModifiers::NONE, used),
            _ => ParseResult::Discard(used),
        },
        _ => ParseResult::Discard(used),
    }
}

fn modified_key(code: KeyCode, parameters: &str, used: usize) -> ParseResult {
    let modifiers = parameters
        .split(';')
        .nth(1)
        .map(parse_modifiers)
        .unwrap_or(KeyModifiers::NONE);
    event(code, modifiers, used)
}

fn parse_csi_u(parameters: &str, used: usize) -> ParseResult {
    let mut fields = parameters.split(';');
    let mut codes = fields.next().unwrap_or_default().split(':');
    let primary = parse_char(codes.next());
    let shifted = parse_char(codes.next());
    let base = parse_char(codes.next());
    let modifier_field = fields.next().unwrap_or("1");
    let modifiers = parse_modifiers(modifier_field);
    let state = parse_state(modifier_field);

    let code = if let Some(base) = base {
        KeyCode::Char(apply_us_modifiers(base, modifiers, state))
    } else if let Some(shifted) = shifted.filter(|_| modifiers.contains(KeyModifiers::SHIFT)) {
        KeyCode::Char(shifted)
    } else {
        match primary {
            Some('\u{1b}') => KeyCode::Esc,
            Some('\r' | '\n') => KeyCode::Enter,
            Some('\t') => KeyCode::Tab,
            Some('\u{7f}') => KeyCode::Backspace,
            Some(character) => KeyCode::Char(character),
            None => return ParseResult::Discard(used),
        }
    };
    ParseResult::Event(
        KeyEvent::new_with_kind_and_state(
            code,
            modifiers,
            crossterm::event::KeyEventKind::Press,
            state,
        ),
        used,
    )
}

fn parse_char(value: Option<&str>) -> Option<char> {
    value
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<u32>().ok())
        .and_then(char::from_u32)
}

fn parse_modifiers(value: &str) -> KeyModifiers {
    let mask = value
        .split(':')
        .next()
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(1)
        .saturating_sub(1);
    let mut modifiers = KeyModifiers::NONE;
    modifiers.set(KeyModifiers::SHIFT, mask & 1 != 0);
    modifiers.set(KeyModifiers::ALT, mask & 2 != 0);
    modifiers.set(KeyModifiers::CONTROL, mask & 4 != 0);
    modifiers.set(KeyModifiers::SUPER, mask & 8 != 0);
    modifiers.set(KeyModifiers::HYPER, mask & 16 != 0);
    modifiers.set(KeyModifiers::META, mask & 32 != 0);
    modifiers
}

fn parse_state(value: &str) -> KeyEventState {
    let mask = value
        .split(':')
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(1)
        .saturating_sub(1);
    let mut state = KeyEventState::NONE;
    state.set(KeyEventState::CAPS_LOCK, mask & 64 != 0);
    state.set(KeyEventState::NUM_LOCK, mask & 128 != 0);
    state
}

fn apply_us_modifiers(base: char, modifiers: KeyModifiers, state: KeyEventState) -> char {
    let shifted = modifiers.contains(KeyModifiers::SHIFT);
    if base.is_ascii_alphabetic() {
        let uppercase = shifted ^ state.contains(KeyEventState::CAPS_LOCK);
        return if uppercase {
            base.to_ascii_uppercase()
        } else {
            base.to_ascii_lowercase()
        };
    }
    if !shifted {
        return base;
    }
    match base {
        '`' => '~',
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        _ => base,
    }
}

fn parse_utf8(bytes: &[u8], modifiers: KeyModifiers, prefix: usize) -> ParseResult {
    let width = match bytes.first().copied() {
        Some(0x00..=0x7f) => 1,
        Some(0xc2..=0xdf) => 2,
        Some(0xe0..=0xef) => 3,
        Some(0xf0..=0xf4) => 4,
        Some(_) => return ParseResult::Discard(prefix + 1),
        None => return ParseResult::Incomplete,
    };
    if bytes.len() < width {
        return ParseResult::Incomplete;
    }
    let Ok(text) = std::str::from_utf8(&bytes[..width]) else {
        return ParseResult::Discard(prefix + width);
    };
    let character = text.chars().next().expect("one complete UTF-8 character");
    let mut modifiers = modifiers;
    if character.is_uppercase() {
        modifiers |= KeyModifiers::SHIFT;
    }
    event(KeyCode::Char(character), modifiers, prefix + width)
}

fn event(code: KeyCode, modifiers: KeyModifiers, used: usize) -> ParseResult {
    ParseResult::Event(KeyEvent::new(code, modifiers), used)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(input: &[u8]) -> KeyEvent {
        match parse_one(input, true) {
            ParseResult::Event(event, used) => {
                assert_eq!(used, input.len());
                event
            }
            _ => panic!("input did not decode to a key event"),
        }
    }

    #[test]
    fn russian_layout_uses_us_base_key() {
        // Cyrillic о occupies the J key on the standard Russian layout.
        let key = decode(b"\x1b[1086::106;1u");
        assert_eq!(key.code, KeyCode::Char('j'));
    }

    #[test]
    fn base_key_support_is_not_language_specific() {
        // The primary codepoint is Arabic; the terminal reports physical S.
        let key = decode(b"\x1b[1587::115;1u");
        assert_eq!(key.code, KeyCode::Char('s'));
    }

    #[test]
    fn ctrl_c_uses_base_key_in_non_latin_layout() {
        let key = decode(b"\x1b[1089::99;5u");
        assert_eq!(key.code, KeyCode::Char('c'));
        assert!(key.modifiers.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn shift_and_caps_lock_normalize_ascii_commands() {
        assert_eq!(decode(b"\x1b[1089:1057:99;2u").code, KeyCode::Char('C'));
        assert_eq!(decode(b"\x1b[1089::99;65u").code, KeyCode::Char('C'));
        assert_eq!(decode(b"\x1b[1089:1057:99;66u").code, KeyCode::Char('c'));
    }

    #[test]
    fn shifted_us_punctuation_produces_help_key() {
        let key = decode(b"\x1b[46:44:47;2u");
        assert_eq!(key.code, KeyCode::Char('?'));
    }

    #[test]
    fn legacy_keys_remain_supported() {
        assert_eq!(decode(b"j").code, KeyCode::Char('j'));
        assert_eq!(decode("о".as_bytes()).code, KeyCode::Char('о'));
        assert_eq!(decode(b"\x1b[A").code, KeyCode::Up);
        assert_eq!(decode(b"\r").code, KeyCode::Enter);
        assert_eq!(decode(b"\x1b[Z").code, KeyCode::BackTab);
        assert_eq!(decode(b"\x1b").code, KeyCode::Esc);
    }

    #[test]
    fn decoder_waits_for_partial_sequences_and_drains_multiple_keys() {
        assert!(matches!(
            parse_one(b"\x1b[1086", false),
            ParseResult::Incomplete
        ));

        let mut decoder = Decoder::default();
        decoder.push(b"\x1b[1086::106;1ujk");
        let events = decoder.drain(Instant::now());
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].code, KeyCode::Char('j'));
        assert_eq!(events[1].code, KeyCode::Char('j'));
        assert_eq!(events[2].code, KeyCode::Char('k'));
    }

    #[test]
    fn protocol_sequences_are_balanced() {
        assert_eq!(PUSH_KEYBOARD_PROTOCOL, "\x1b[>13u");
        assert_eq!(POP_KEYBOARD_PROTOCOL, "\x1b[<1u");
    }
}
