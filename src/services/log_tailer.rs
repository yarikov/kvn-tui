use chrono::{DateTime, FixedOffset, Local, TimeZone};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

/// Append an app-generated log line to the application log file.
/// Format matches sing-box style: `+HHMM YYYY-MM-DD HH:MM:SS LEVEL message`
pub fn append_app_log(level: &str, message: &str) {
    let now = Local::now();
    let path = crate::paths::app_log_path();
    if let Ok(mut file) = OpenOptions::new().append(true).create(true).open(&path) {
        let _ = writeln!(
            file,
            "{} {} {} {}",
            now.format("%z"),
            now.format("%Y-%m-%d %H:%M:%S"),
            level,
            message
        );
    }
}

/// Tails log files and returns new lines formatted for the TUI.
/// Each line is tagged with its source and shows a short time:
/// `[tag] HH:MM:SS LEVEL message`
pub struct LogTailer {
    files: Vec<(PathBuf, &'static str, u64)>,
}

impl LogTailer {
    pub fn new(files: Vec<(PathBuf, &'static str)>) -> Self {
        let files = files
            .into_iter()
            .map(|(p, tag)| {
                let pos = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                (p, tag, pos)
            })
            .collect();
        Self { files }
    }

    #[cfg(test)]
    pub fn test_new(files: Vec<(PathBuf, &'static str)>) -> Self {
        let files = files.into_iter().map(|(p, tag)| (p, tag, 0)).collect();
        Self { files }
    }

    pub fn tail(&mut self) -> Vec<String> {
        let mut entries: Vec<(DateTime<FixedOffset>, String)> = Vec::new();

        for (path, tag, pos) in self.files.iter_mut() {
            let Ok(mut file) = File::open(path) else {
                continue;
            };
            let Ok(metadata) = file.metadata() else {
                continue;
            };
            let file_len = metadata.len();

            if *pos > file_len {
                *pos = 0;
            }

            if file.seek(SeekFrom::Start(*pos)).is_ok() {
                let mut reader = BufReader::new(file);
                for line in reader.by_ref().lines().map_while(Result::ok) {
                    if line.trim().is_empty() {
                        continue;
                    }
                    entries.push(format_entry(&line, tag));
                }
                if let Ok(new_pos) = reader.stream_position() {
                    *pos = new_pos;
                }
            }
        }

        entries.sort_by_key(|a| a.0);
        entries.into_iter().map(|(_, line)| line).collect()
    }

    /// Load at most `max_lines` entries written since the supplied byte
    /// offsets, then continue tailing from the current ends of the files.
    /// A truncated/rotated file starts at byte zero for the new session.
    pub fn load_history(&mut self, start_positions: &[u64], max_lines: usize) -> Vec<String> {
        if max_lines == 0 {
            return Vec::new();
        }

        let mut entries = Vec::new();
        for (index, (path, tag, pos)) in self.files.iter_mut().enumerate() {
            let Ok(mut file) = File::open(path) else {
                continue;
            };
            let Ok(metadata) = file.metadata() else {
                continue;
            };
            let file_len = metadata.len();
            let requested_start = start_positions.get(index).copied().unwrap_or(file_len);
            let start = if requested_start <= file_len {
                requested_start
            } else {
                0
            };

            for line in read_last_lines(&mut file, start, file_len, max_lines) {
                if line.trim().is_empty() {
                    continue;
                }
                entries.push(format_entry(&line, tag));
            }
            *pos = file_len;
        }

        entries.sort_by_key(|entry| entry.0);
        let keep_from = entries.len().saturating_sub(max_lines);
        entries
            .into_iter()
            .skip(keep_from)
            .map(|(_, line)| line)
            .collect()
    }
}

fn format_entry(line: &str, tag: &str) -> (DateTime<FixedOffset>, String) {
    let parsed = parse_timestamp(line);
    let formatted = if let Some((dt, prefix_len)) = parsed {
        let remainder = line.get(prefix_len..).unwrap_or_default();
        format!("{} {}{}", tag, dt.format("%H:%M:%S"), remainder)
    } else {
        format!("{} {}", tag, line)
    };
    let sort_key = parsed.map(|(dt, _)| dt).unwrap_or_else(|| {
        FixedOffset::east_opt(0)
            .unwrap()
            .from_utc_datetime(&Local::now().naive_local())
    });
    (sort_key, formatted)
}

fn read_last_lines(file: &mut File, start: u64, end: u64, limit: usize) -> Vec<String> {
    const CHUNK_SIZE: u64 = 8 * 1024;

    let mut cursor = end;
    let mut newline_count = 0;
    let mut chunks = Vec::new();
    while cursor > start && newline_count <= limit {
        let chunk_start = cursor.saturating_sub(CHUNK_SIZE).max(start);
        let mut chunk = vec![0; (cursor - chunk_start) as usize];
        if file.seek(SeekFrom::Start(chunk_start)).is_err() || file.read_exact(&mut chunk).is_err()
        {
            return Vec::new();
        }
        newline_count += chunk.iter().filter(|byte| **byte == b'\n').count();
        chunks.push(chunk);
        cursor = chunk_start;
    }

    chunks.reverse();
    let bytes = chunks.concat();
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<_> = text.lines().collect();
    lines[lines.len().saturating_sub(limit)..]
        .iter()
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect()
}

/// Parse a sing-box style timestamp from the start of a line.
/// Returns the parsed value and the byte length of the timestamp prefix.
fn parse_timestamp(line: &str) -> Option<(DateTime<FixedOffset>, usize)> {
    // Try the longer form first. Its first 25 bytes are also a valid seconds
    // timestamp, so reversing this order would leave `.123` in the rendered
    // message. `str::get` makes malformed Unicode boundaries a normal miss
    // instead of a slicing panic.
    if let Some(prefix) = line.get(..29)
        && let Ok(dt) = DateTime::parse_from_str(prefix, "%z %Y-%m-%d %H:%M:%S%.3f")
    {
        return Some((dt, 29));
    }
    if let Some(prefix) = line.get(..25)
        && let Ok(dt) = DateTime::parse_from_str(prefix, "%z %Y-%m-%d %H:%M:%S")
    {
        return Some((dt, 25));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn tail_reads_new_lines() {
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        writeln!(temp, "+0000 2024-01-01 00:00:00 INFO log line 1").unwrap();
        writeln!(temp, "+0000 2024-01-01 00:00:01 INFO log line 2").unwrap();
        let path = temp.path().to_path_buf();

        let mut tailer = LogTailer::test_new(vec![(path, "[app]")]);
        let lines = tailer.tail();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("[app] 00:00:00 INFO log line 1"));
        assert!(lines[1].starts_with("[app] 00:00:01 INFO log line 2"));
    }

    #[test]
    fn tail_preserves_lines_as_is() {
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        writeln!(temp, "+0000 2024-01-01 00:00:00 INFO hello").unwrap();
        writeln!(temp, "+0000 2024-01-01 00:00:01 WARN plain line").unwrap();
        let path = temp.path().to_path_buf();

        let mut tailer = LogTailer::test_new(vec![(path, "[app]")]);
        let lines = tailer.tail();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("[app] 00:00:00 INFO hello"));
        assert!(lines[1].starts_with("[app] 00:00:01 WARN plain line"));
    }

    #[test]
    fn tail_resets_on_rotation() {
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            temp,
            "+0000 2024-01-01 00:00:00 INFO this is a long old log line"
        )
        .unwrap();
        let path = temp.path().to_path_buf();

        let mut tailer = LogTailer::test_new(vec![(path.clone(), "[app]")]);
        let lines = tailer.tail();
        assert_eq!(lines.len(), 1);

        // Simulate rotation: file shrinks
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "+0000 2024-01-01 00:00:00 INFO new").unwrap();
        drop(file);

        let lines = tailer.tail();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("[app] 00:00:00 INFO new"));
    }

    #[test]
    fn tail_merges_two_files_chronologically() {
        let mut temp1 = tempfile::NamedTempFile::new().unwrap();
        let mut temp2 = tempfile::NamedTempFile::new().unwrap();

        writeln!(temp1, "+0000 2024-01-01 00:00:02 INFO from file 1").unwrap();
        writeln!(temp1, "+0000 2024-01-01 00:00:04 INFO from file 1 again").unwrap();

        writeln!(temp2, "+0000 2024-01-01 00:00:01 INFO from file 2").unwrap();
        writeln!(temp2, "+0000 2024-01-01 00:00:03 INFO from file 2 again").unwrap();

        let path1 = temp1.path().to_path_buf();
        let path2 = temp2.path().to_path_buf();

        let mut tailer = LogTailer::test_new(vec![(path1, "[app]"), (path2, "[sb]")]);
        let lines = tailer.tail();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("[sb] 00:00:01"));
        assert!(lines[1].starts_with("[app] 00:00:02"));
        assert!(lines[2].starts_with("[sb] 00:00:03"));
        assert!(lines[3].starts_with("[app] 00:00:04"));
    }

    #[test]
    fn history_starts_at_daemon_offsets_and_keeps_global_tail() {
        let mut app = tempfile::NamedTempFile::new().unwrap();
        let mut singbox = tempfile::NamedTempFile::new().unwrap();
        writeln!(app, "+0000 2024-01-01 00:00:00 INFO old app").unwrap();
        writeln!(singbox, "+0000 2024-01-01 00:00:00 INFO old sing-box").unwrap();
        app.flush().unwrap();
        singbox.flush().unwrap();
        let offsets = [
            app.as_file().metadata().unwrap().len(),
            singbox.as_file().metadata().unwrap().len(),
        ];

        writeln!(app, "+0000 2024-01-01 00:00:01 INFO app one").unwrap();
        writeln!(singbox, "+0000 2024-01-01 00:00:02 INFO sb two").unwrap();
        writeln!(app, "+0000 2024-01-01 00:00:03 INFO app three").unwrap();
        writeln!(singbox, "+0000 2024-01-01 00:00:04 INFO sb four").unwrap();
        app.flush().unwrap();
        singbox.flush().unwrap();

        let mut tailer = LogTailer::new(vec![
            (app.path().to_path_buf(), "[app]"),
            (singbox.path().to_path_buf(), "[sb]"),
        ]);
        assert_eq!(
            tailer.load_history(&offsets, 3),
            vec![
                "[sb] 00:00:02 INFO sb two",
                "[app] 00:00:03 INFO app three",
                "[sb] 00:00:04 INFO sb four",
            ]
        );

        writeln!(app, "+0000 2024-01-01 00:00:05 INFO appended").unwrap();
        app.flush().unwrap();
        assert_eq!(tailer.tail(), vec!["[app] 00:00:05 INFO appended"]);
    }

    #[test]
    fn history_reads_rotated_file_from_its_new_start() {
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        writeln!(temp, "+0000 2024-01-01 00:00:01 INFO new session").unwrap();
        temp.flush().unwrap();

        let mut tailer = LogTailer::new(vec![(temp.path().to_path_buf(), "[app]")]);
        assert_eq!(
            tailer.load_history(&[u64::MAX], 1000),
            vec!["[app] 00:00:01 INFO new session"]
        );
    }

    #[test]
    fn tail_tags_untimestamped_lines() {
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        writeln!(temp, "plain line without timestamp").unwrap();
        let path = temp.path().to_path_buf();

        let mut tailer = LogTailer::test_new(vec![(path, "[app]")]);
        let lines = tailer.tail();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("[app] plain line"));
    }

    #[test]
    fn parse_timestamp_valid() {
        use chrono::Datelike;
        let line = "+0300 2026-06-09 21:28:34 INFO hello";
        let (dt, prefix_len) = parse_timestamp(line).unwrap();
        assert_eq!(dt.year(), 2026);
        assert_eq!(dt.month(), 6);
        assert_eq!(dt.day(), 9);
        assert_eq!(prefix_len, 25);
    }

    #[test]
    fn parse_timestamp_from_real_singbox_line() {
        use chrono::Timelike;
        let line = "+0300 2026-06-10 09:35:55 DEBUG [4216981911 0ms] router: match[1] inbound=tun-in port=53 => hijack-dns";
        let (dt, prefix_len) = parse_timestamp(line).unwrap();
        assert_eq!(dt.hour(), 9);
        assert_eq!(dt.minute(), 35);
        assert_eq!(dt.second(), 55);
        assert_eq!(prefix_len, 25);
    }

    #[test]
    fn parse_timestamp_returns_none_for_malformed() {
        let line = "some random text without timestamp";
        assert!(parse_timestamp(line).is_none());
    }

    #[test]
    fn tail_handles_unicode_crossing_timestamp_byte_boundary() {
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        writeln!(temp, "aaaaaaaaaaaaaaaaaaaaaaaaé message").unwrap();
        let path = temp.path().to_path_buf();

        let mut tailer = LogTailer::test_new(vec![(path, "[sb]")]);
        assert_eq!(
            tailer.tail(),
            vec!["[sb] aaaaaaaaaaaaaaaaaaaaaaaaé message"]
        );
    }

    #[test]
    fn tail_strips_millisecond_timestamp_without_leaving_fraction() {
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        writeln!(temp, "+0300 2026-06-09 21:28:34.123 INFO hello").unwrap();
        let path = temp.path().to_path_buf();

        let mut tailer = LogTailer::test_new(vec![(path, "[sb]")]);
        assert_eq!(tailer.tail(), vec!["[sb] 21:28:34 INFO hello"]);
    }
}
