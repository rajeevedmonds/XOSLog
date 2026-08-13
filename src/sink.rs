//! Output destinations (sinks) for formatted log records.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// A destination for formatted log records.
///
/// Implementations must be [`Send`] because they are moved into the writer
/// thread.
pub trait Sink: Send {
    /// Write raw bytes to the destination.
    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// Flush any buffered data to the underlying destination.
    fn flush(&mut self) -> io::Result<()>;
}

/// A sink that writes to standard output.
pub struct StdoutSink;

impl Sink for StdoutSink {
    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut out = io::stdout().lock();
        out.write_all(bytes)?;
        out.flush()
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().lock().flush()
    }
}

/// A sink that writes to standard error.
pub struct StderrSink;

impl Sink for StderrSink {
    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut out = io::stderr().lock();
        out.write_all(bytes)?;
        out.flush()
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stderr().lock().flush()
    }
}

/// A file sink with size-based rotation.
///
/// The active log is written to `path`. Once the active file grows past
/// `max_size` bytes it is rotated: the current file is renamed to `path.1`,
/// the previous `path.1` becomes `path.2`, and so on, up to `path.<max_backups>`.
/// Older backups are removed. Setting `max_backups` to `0` disables backups and
/// truncates the active file in place on rotation. Setting `max_size` to `0`
/// disables rotation entirely.
pub struct FileSink {
    path: PathBuf,
    file: File,
    max_size: u64,
    max_backups: usize,
    size: u64,
}

impl FileSink {
    /// Open (or create) a log file at `path`, creating parent directories as
    /// needed. Existing content is kept (the file is opened in append mode).
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if the file or its parent directory cannot be
    /// created.
    pub fn open(path: impl AsRef<Path>, max_size: u64, max_backups: usize) -> io::Result<FileSink> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(FileSink {
            path,
            file,
            max_size,
            max_backups,
            size,
        })
    }

    /// Rotate the current log file.
    ///
    /// On success the sink points at a fresh, empty active file.
    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let _ = self.file.sync_all();
        if self.max_backups > 0 {
            self.shift_backups()?;
            self.file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
        } else {
            self.file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&self.path)?;
        }
        self.size = 0;
        Ok(())
    }

    /// Rename `path` to `path.1`, shift the existing backups up one slot and
    /// drop the oldest backup.
    fn shift_backups(&self) -> io::Result<()> {
        let oldest = backup_path(&self.path, self.max_backups);
        let _ = std::fs::remove_file(&oldest);
        for slot in (1..self.max_backups).rev() {
            let src = backup_path(&self.path, slot);
            let dst = backup_path(&self.path, slot + 1);
            let _ = std::fs::rename(&src, &dst);
        }
        std::fs::rename(&self.path, backup_path(&self.path, 1))
    }
}

impl Sink for FileSink {
    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.max_size > 0 && self.size.saturating_add(bytes.len() as u64) > self.max_size {
            self.rotate()?;
        }
        self.file.write_all(bytes)?;
        self.size += bytes.len() as u64;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Compute the path of the `n`-th rotated backup file.
fn backup_path(path: &Path, n: usize) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(format!(".{n}"));
    PathBuf::from(os)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("xoslog-sink-test-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn writes_and_flushes() {
        let path = temp_file();
        let mut sink = FileSink::open(&path, 0, 0).unwrap();
        sink.write_bytes(b"hello\n").unwrap();
        sink.write_bytes(b"world\n").unwrap();
        sink.flush().unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello\nworld\n");
    }

    #[test]
    fn rotation_creates_backups() {
        let path = temp_file();
        let mut sink = FileSink::open(&path, 20, 3).unwrap();
        for i in 0..10 {
            sink.write_bytes(format!("record-{i}-xxxxxxxxxxxx\n").as_bytes())
                .unwrap();
        }
        sink.flush().unwrap();
        assert!(path.exists());
        assert!(backup_path(&path, 1).exists());
        assert!(backup_path(&path, 2).exists());
        assert!(backup_path(&path, 3).exists());
        assert!(backup_path(&path, 4).metadata().is_err());
    }

    #[test]
    fn rotation_truncates_when_no_backups() {
        let path = temp_file();
        let mut sink = FileSink::open(&path, 20, 0).unwrap();
        for i in 0..10 {
            sink.write_bytes(format!("record-{i}-xxxxxxxxxxxx\n").as_bytes())
                .unwrap();
        }
        sink.flush().unwrap();
        assert!(backup_path(&path, 1).metadata().is_err());
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size <= 40);
    }
}
