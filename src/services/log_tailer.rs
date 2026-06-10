use chrono::{DateTime, FixedOffset, Local, TimeZone};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

/// Append an app-generated log line to the application log file.
/// Format matches sing-box style: `+HHMM YYYY-MM-DD HH:MM:SS LEVEL message`
pub fn append_app_log(level: &str, message: &str) {
    let now = Local::now();
    let path = crate::infra::paths::app_log_path();
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
                    let ts = parse_timestamp(&line);
                    let formatted = if let Some(dt) = ts {
                        format!("{} {}{}", tag, dt.format("%H:%M:%S"), &line[25..])
                    } else {
                        format!("{} {}", tag, line)
                    };
                    let sort_key = ts.unwrap_or_else(|| {
                        FixedOffset::east_opt(0)
                            .unwrap()
                            .from_utc_datetime(&Local::now().naive_local())
                    });
                    entries.push((sort_key, formatted));
                }
                if let Ok(new_pos) = reader.stream_position() {
                    *pos = new_pos;
                }
            }
        }

        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries.into_iter().map(|(_, line)| line).collect()
    }
}

/// Parse a sing-box style timestamp from the start of a line.
/// Returns `None` if no timestamp is found.
fn parse_timestamp(line: &str) -> Option<DateTime<FixedOffset>> {
    if line.len() >= 25 {
        let prefix = &line[..25];
        if let Ok(dt) = DateTime::parse_from_str(prefix, "%z %Y-%m-%d %H:%M:%S") {
            return Some(dt);
        }
        // Try with milliseconds: +0300 2026-06-09 21:28:34.123
        if line.len() >= 29 {
            let prefix_ms = &line[..29];
            if let Ok(dt) = DateTime::parse_from_str(prefix_ms, "%z %Y-%m-%d %H:%M:%S%.3f") {
                return Some(dt);
            }
        }
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
        writeln!(temp, "+0000 2024-01-01 00:00:00 INFO this is a long old log line").unwrap();
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
        let dt = parse_timestamp(line).unwrap();
        assert_eq!(dt.year(), 2026);
        assert_eq!(dt.month(), 6);
        assert_eq!(dt.day(), 9);
    }

    #[test]
    fn parse_timestamp_from_real_singbox_line() {
        use chrono::Timelike;
        let line = "+0300 2026-06-10 09:35:55 DEBUG [4216981911 0ms] router: match[1] inbound=tun-in port=53 => hijack-dns";
        let dt = parse_timestamp(line).unwrap();
        assert_eq!(dt.hour(), 9);
        assert_eq!(dt.minute(), 35);
        assert_eq!(dt.second(), 55);
    }

    #[test]
    fn parse_timestamp_returns_none_for_malformed() {
        let line = "some random text without timestamp";
        assert!(parse_timestamp(line).is_none());
    }
}
