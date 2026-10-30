//! Output destinations (sinks) for formatted log records.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::entry::LogEntry;

/// A destination for formatted log records.
///
/// Implementations must be [`Send`] because they are moved into the writer
/// thread.
pub trait Sink: Send {
    /// Write raw bytes to the destination.
    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Write a full log entry. The default implementation forwards the
    /// pre-formatted record to [`write_bytes`]. Sinks that need structured
    /// per-record data (e.g. the syslog severity) override this method.
    fn write_entry(&mut self, _entry: &LogEntry, formatted: &[u8]) -> io::Result<()> {
        self.write_bytes(formatted)
    }

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

/// How often a [`FileSink`] rotates on the wall clock, in addition to any
/// size threshold.
///
/// Intervals are aligned to the Unix epoch so every process sharing the same
/// interval rotates at the same UTC boundary: hourly at `:00`, daily at
/// `00:00`, weekly at the Monday `00:00` that starts an ISO week. A custom
/// duration is aligned to multiples of that duration since the epoch.
///
/// Time-based rotation is checked before each write. Setting
/// [`RotationInterval::Never`] (the default) leaves only size-based rotation
/// in effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationInterval {
    /// Do not rotate on a schedule. Size-based rotation still applies when
    /// `max_size` is non-zero.
    Never,
    /// Rotate at the top of every UTC hour.
    Hourly,
    /// Rotate at UTC midnight.
    Daily,
    /// Rotate at the start of every ISO week (Monday 00:00 UTC).
    Weekly,
    /// Rotate every `duration`. Durations of zero are treated as
    /// [`RotationInterval::Never`].
    Every(Duration),
}

impl RotationInterval {
    /// Seconds in one hour.
    const HOUR_SECS: u64 = 3600;
    /// Seconds in one day.
    const DAY_SECS: u64 = 86_400;
    /// Seconds in one ISO week.
    const WEEK_SECS: u64 = 604_800;

    /// Length of this interval in seconds, or `None` when rotation is off.
    fn period_secs(self) -> Option<u64> {
        match self {
            RotationInterval::Never => None,
            RotationInterval::Hourly => Some(Self::HOUR_SECS),
            RotationInterval::Daily => Some(Self::DAY_SECS),
            RotationInterval::Weekly => Some(Self::WEEK_SECS),
            RotationInterval::Every(duration) => {
                let secs = duration.as_secs();
                if secs == 0 {
                    None
                } else {
                    Some(secs)
                }
            }
        }
    }

    /// Unix-epoch second at which the interval containing `now` began.
    fn slot_start(self, now: SystemTime) -> Option<u64> {
        let secs = unix_secs(now);
        match self {
            RotationInterval::Never => None,
            RotationInterval::Hourly | RotationInterval::Daily | RotationInterval::Every(_) => {
                let period = self.period_secs()?;
                Some(secs / period * period)
            }
            RotationInterval::Weekly => Some(iso_week_start_secs(secs)),
        }
    }
}

impl Default for RotationInterval {
    fn default() -> Self {
        RotationInterval::Never
    }
}

/// Seconds since the Unix epoch. Times before the epoch clamp to `0`.
fn unix_secs(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

/// Unix-epoch second of the ISO-week Monday 00:00 UTC that contains `secs`.
///
/// The Unix epoch is a Thursday, so a naive `secs / week * week` alignment
/// would rotate on Thursdays. ISO weeks start Monday; the Monday of the week
/// that contains the epoch is 1969-12-29 (`-3` days). Negative results (the
/// three days before 1970-01-01) clamp to `0`.
fn iso_week_start_secs(secs: u64) -> u64 {
    const EPOCH_MONDAY_OFFSET: i128 = 3 * 86_400;
    const WEEK: i128 = 604_800;
    let shifted = secs as i128 + EPOCH_MONDAY_OFFSET;
    let start = shifted.div_euclid(WEEK) * WEEK - EPOCH_MONDAY_OFFSET;
    if start < 0 {
        0
    } else {
        start as u64
    }
}

/// Slot the active file belongs to: the file's mtime slot when that is
/// known (so a leftover file from an earlier interval rotates on the first
/// write), otherwise the slot containing `clock.now()`. Frozen clocks skip
/// mtime so tests stay deterministic.
fn interval_start_for(file: &File, interval: RotationInterval, clock: &Clock) -> Option<u64> {
    let now_slot = interval.slot_start(clock.now())?;
    match clock {
        Clock::System => file
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| interval.slot_start(t))
            .or(Some(now_slot)),
        #[cfg(test)]
        Clock::Frozen(_) => Some(now_slot),
    }
}

/// Source of wall-clock time used by a [`FileSink`] to decide when an
/// interval has elapsed.
///
/// Production sinks use the system clock. Tests inject a frozen instant so
/// rotation can be exercised without waiting for a real hour or day to pass.
#[derive(Clone)]
enum Clock {
    /// Read [`SystemTime::now`] on every check.
    System,
    /// Always report this frozen instant.
    #[cfg(test)]
    Frozen(SystemTime),
}

impl Clock {
    /// The current instant according to this clock.
    fn now(&self) -> SystemTime {
        match self {
            Clock::System => SystemTime::now(),
            #[cfg(test)]
            Clock::Frozen(instant) => *instant,
        }
    }
}

/// A file sink with size-based and optional time-based rotation.
///
/// The active log is written to `path`. Rotation is triggered when either:
///
/// - the active file would grow past `max_size` bytes (`0` disables size
///   rotation), or
/// - a configured [`RotationInterval`] has elapsed since the current file's
///   interval slot began.
///
/// On rotation the current file is renamed to `path.1`, the previous `path.1`
/// becomes `path.2`, and so on, up to `path.<max_backups>`. Older backups are
/// removed. Setting `max_backups` to `0` disables backups and truncates the
/// active file in place on rotation.
pub struct FileSink {
    path: PathBuf,
    file: File,
    max_size: u64,
    max_backups: usize,
    size: u64,
    interval: RotationInterval,
    /// Start of the interval slot the current active file belongs to.
    /// `None` when time-based rotation is disabled.
    interval_start: Option<u64>,
    clock: Clock,
}

impl FileSink {
    /// Open (or create) a log file at `path`, creating parent directories as
    /// needed. Existing content is kept (the file is opened in append mode).
    /// Size-based rotation uses `max_size` and `max_backups`; time-based
    /// rotation is off until [`FileSink::rotate_every`] is called.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if the file or its parent directory cannot be
    /// created.
    pub fn open(path: impl AsRef<Path>, max_size: u64, max_backups: usize) -> io::Result<FileSink> {
        Self::open_with(
            path,
            max_size,
            max_backups,
            RotationInterval::Never,
            Clock::System,
        )
    }

    /// Open a log file that also rotates on `interval`.
    ///
    /// Size-based rotation still applies when `max_size` is non-zero; either
    /// trigger is enough to rotate. Pass [`RotationInterval::Never`] for
    /// size-only behaviour, matching [`FileSink::open`].
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if the file or its parent directory cannot be
    /// created.
    pub fn open_with_interval(
        path: impl AsRef<Path>,
        max_size: u64,
        max_backups: usize,
        interval: RotationInterval,
    ) -> io::Result<FileSink> {
        Self::open_with(path, max_size, max_backups, interval, Clock::System)
    }

    /// Enable (or disable) time-based rotation on an already-opened sink.
    ///
    /// The current interval slot is captured from the clock so the next write
    /// in a later slot rotates. Passing [`RotationInterval::Never`] turns
    /// schedule-based rotation off without affecting size rotation.
    pub fn rotate_every(mut self, interval: RotationInterval) -> FileSink {
        self.interval = interval;
        self.interval_start = interval.slot_start(self.clock.now());
        self
    }

    /// Freeze the sink's clock at `instant` so tests can advance time without
    /// waiting for a real interval to elapse.
    #[cfg(test)]
    fn with_clock(mut self, instant: SystemTime) -> FileSink {
        self.clock = Clock::Frozen(instant);
        self.interval_start = self.interval.slot_start(instant);
        self
    }

    /// Advance a frozen clock to `instant`. No-op for the system clock.
    #[cfg(test)]
    fn set_now(&mut self, instant: SystemTime) {
        self.clock = Clock::Frozen(instant);
    }

    /// Open a log file with an explicit interval and clock.
    fn open_with(
        path: impl AsRef<Path>,
        max_size: u64,
        max_backups: usize,
        interval: RotationInterval,
        clock: Clock,
    ) -> io::Result<FileSink> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let interval_start = interval_start_for(&file, interval, &clock);
        Ok(FileSink {
            path,
            file,
            max_size,
            max_backups,
            size,
            interval,
            interval_start,
            clock,
        })
    }

    /// Whether the current interval slot has ended according to `clock`.
    fn interval_elapsed(&self) -> bool {
        match (
            self.interval_start,
            self.interval.slot_start(self.clock.now()),
        ) {
            (Some(opened), Some(now)) => now > opened,
            _ => false,
        }
    }

    /// Rotate the current log file.
    ///
    /// On success the sink points at a fresh, empty active file whose
    /// interval slot is the one containing the current clock instant.
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
        self.interval_start = self.interval.slot_start(self.clock.now());
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
        let size_hit =
            self.max_size > 0 && self.size.saturating_add(bytes.len() as u64) > self.max_size;
        if size_hit || self.interval_elapsed() {
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

    #[test]
    fn hourly_slot_aligns_to_epoch() {
        let t = UNIX_EPOCH + Duration::from_secs(3_600 + 59);
        assert_eq!(RotationInterval::Hourly.slot_start(t), Some(3_600));
        let next = UNIX_EPOCH + Duration::from_secs(7_200);
        assert_eq!(RotationInterval::Hourly.slot_start(next), Some(7_200));
    }

    #[test]
    fn daily_slot_aligns_to_midnight() {
        let t = UNIX_EPOCH + Duration::from_secs(86_400 + 12 * 3_600);
        assert_eq!(RotationInterval::Daily.slot_start(t), Some(86_400));
    }

    #[test]
    fn weekly_slot_is_monday() {
        // Unix epoch is Thursday; 4 days later is Monday 00:00.
        let monday = UNIX_EPOCH + Duration::from_secs(4 * 86_400);
        assert_eq!(
            RotationInterval::Weekly.slot_start(monday),
            Some(4 * 86_400)
        );
        let wednesday = monday + Duration::from_secs(2 * 86_400 + 3_600);
        assert_eq!(
            RotationInterval::Weekly.slot_start(wednesday),
            Some(4 * 86_400)
        );
    }

    #[test]
    fn never_and_zero_duration_have_no_slot() {
        assert_eq!(RotationInterval::Never.slot_start(SystemTime::now()), None);
        assert_eq!(
            RotationInterval::Every(Duration::from_secs(0)).slot_start(SystemTime::now()),
            None
        );
    }

    #[test]
    fn time_rotation_creates_backup_when_interval_elapses() {
        let path = temp_file();
        let t0 = UNIX_EPOCH + Duration::from_secs(3_600);
        let mut sink = FileSink::open(&path, 0, 3)
            .unwrap()
            .rotate_every(RotationInterval::Hourly)
            .with_clock(t0);
        sink.write_bytes(b"first\n").unwrap();
        assert!(!backup_path(&path, 1).exists());

        sink.set_now(t0 + Duration::from_secs(3_600));
        sink.write_bytes(b"second\n").unwrap();
        sink.flush().unwrap();

        assert!(backup_path(&path, 1).exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\n");
        assert_eq!(
            std::fs::read_to_string(backup_path(&path, 1)).unwrap(),
            "first\n"
        );
    }

    #[test]
    fn time_rotation_does_not_fire_inside_the_same_slot() {
        let path = temp_file();
        let t0 = UNIX_EPOCH + Duration::from_secs(3_600);
        let mut sink = FileSink::open(&path, 0, 3)
            .unwrap()
            .rotate_every(RotationInterval::Hourly)
            .with_clock(t0);
        sink.write_bytes(b"a\n").unwrap();
        sink.set_now(t0 + Duration::from_secs(3_599));
        sink.write_bytes(b"b\n").unwrap();
        sink.flush().unwrap();
        assert!(!backup_path(&path, 1).exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nb\n");
    }

    #[test]
    fn custom_every_interval_rotates() {
        let path = temp_file();
        let t0 = UNIX_EPOCH;
        let mut sink = FileSink::open(&path, 0, 1)
            .unwrap()
            .rotate_every(RotationInterval::Every(Duration::from_secs(10)))
            .with_clock(t0);
        sink.write_bytes(b"old\n").unwrap();
        sink.set_now(t0 + Duration::from_secs(10));
        sink.write_bytes(b"new\n").unwrap();
        sink.flush().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        assert_eq!(
            std::fs::read_to_string(backup_path(&path, 1)).unwrap(),
            "old\n"
        );
    }

    #[test]
    fn leftover_file_from_earlier_slot_rotates_on_first_write() {
        let path = temp_file();
        std::fs::write(&path, b"stale\n").unwrap();
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let past = SystemTime::now() - Duration::from_secs(3_600);
        file.set_modified(past).unwrap();
        drop(file);

        let mut sink = FileSink::open_with_interval(&path, 0, 3, RotationInterval::Hourly).unwrap();
        sink.write_bytes(b"fresh\n").unwrap();
        sink.flush().unwrap();

        assert!(backup_path(&path, 1).exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh\n");
        assert_eq!(
            std::fs::read_to_string(backup_path(&path, 1)).unwrap(),
            "stale\n"
        );
    }

    #[test]
    fn size_and_time_rotation_either_trigger() {
        let path = temp_file();
        let t0 = UNIX_EPOCH + Duration::from_secs(86_400);
        let mut sink = FileSink::open(&path, 8, 2)
            .unwrap()
            .rotate_every(RotationInterval::Daily)
            .with_clock(t0);
        sink.write_bytes(b"1234567\n").unwrap();
        // Size trigger, still inside the same day.
        sink.write_bytes(b"overflow\n").unwrap();
        assert!(backup_path(&path, 1).exists());
        // Time trigger on the next day, even though the new file is small.
        sink.set_now(t0 + Duration::from_secs(86_400));
        sink.write_bytes(b"next-day\n").unwrap();
        sink.flush().unwrap();
        assert!(backup_path(&path, 2).exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "next-day\n");
    }
}
