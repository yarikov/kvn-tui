use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

/// Append an app-generated log line to the shared log file.
pub fn append_app_log(line: &str) {
    let path = crate::infra::paths::singbox_log_path();
    if let Ok(mut file) = OpenOptions::new().append(true).create(true).open(&path) {
        let _ = writeln!(file, "{}", line);
    }
}

/// Tails the sing-box log file and returns new lines.
pub struct LogTailer {
    path: PathBuf,
    pos: u64,
}

impl LogTailer {
    pub fn new(path: PathBuf) -> Self {
        let pos = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self { path, pos }
    }

    #[cfg(test)]
    pub fn test_new(path: PathBuf) -> Self {
        Self { path, pos: 0 }
    }

    pub fn tail(&mut self) -> Vec<String> {
        let Ok(mut file) = File::open(&self.path) else {
            return Vec::new();
        };
        let Ok(metadata) = file.metadata() else {
            return Vec::new();
        };
        let file_len = metadata.len();

        // If file shrank (rotated), reset position.
        if self.pos > file_len {
            self.pos = 0;
        }

        let mut lines = Vec::new();
        if file.seek(SeekFrom::Start(self.pos)).is_ok() {
            let mut reader = BufReader::new(file);
            for line in reader.by_ref().lines().map_while(Result::ok) {
                if line.trim().is_empty() {
                    continue;
                }
                if line.starts_with('[') {
                    lines.push(line);
                } else {
                    lines.push(format!("[sing-box] {}", line));
                }
            }
            // Update position to exact end of read data
            if let Ok(pos) = reader.stream_position() {
                self.pos = pos;
            }
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn tail_reads_new_lines() {
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        writeln!(temp, "log line 1").unwrap();
        writeln!(temp, "log line 2").unwrap();
        let path = temp.path().to_path_buf();

        // Start from beginning for this test
        let mut tailer = LogTailer::test_new(path);
        let lines = tailer.tail();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("log line 1"));
        assert!(lines[1].contains("log line 2"));
    }

    #[test]
    fn tail_preserves_app_prefix() {
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        writeln!(temp, "[app] hello").unwrap();
        writeln!(temp, "[geo] updated").unwrap();
        writeln!(temp, "plain sing-box line").unwrap();
        let path = temp.path().to_path_buf();

        let mut tailer = LogTailer::test_new(path);
        let lines = tailer.tail();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "[app] hello");
        assert_eq!(lines[1], "[geo] updated");
        assert_eq!(lines[2], "[sing-box] plain sing-box line");
    }

    #[test]
    fn tail_resets_on_rotation() {
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        writeln!(temp, "this is a long old log line").unwrap();
        let path = temp.path().to_path_buf();

        // Start from beginning for this test
        let mut tailer = LogTailer::test_new(path.clone());
        let lines = tailer.tail();
        assert_eq!(lines.len(), 1);

        // Simulate rotation: file shrinks
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "new").unwrap();
        drop(file);

        let lines = tailer.tail();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("new"));
    }
}
