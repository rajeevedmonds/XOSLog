//! Syslog (RFC 3164) support.
//!
//! [`SyslogSink`] delivers records to a syslog daemon over a Unix domain
//! datagram socket — the standard `/dev/log` interface on Linux. It is
//! implemented in pure Rust with no dependencies and no `unsafe` code.

use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};

use crate::entry::LogEntry;
use crate::level::Level;
use crate::sink::Sink;
use crate::time::Timestamp;

/// Default Unix datagram socket paths tried, in order, when delivering to the
/// local syslog daemon.
pub const DEFAULT_SYSLOG_SOCKETS: &[&str] = &["/dev/log", "/var/run/syslog", "/var/run/log"];

/// Maximum size of a legacy syslog datagram (RFC 3164).
const MAX_SYSLOG_PACKET: usize = 1024;

/// RFC 3164 severity codes (`syslog.h`).
const SEVERITY_DEBUG: u8 = 7;
const SEVERITY_INFO: u8 = 6;
const SEVERITY_WARNING: u8 = 4;
const SEVERITY_ERROR: u8 = 3;

/// Syslog facility, as defined in RFC 3164 / `syslog.h`.
///
/// The facility selects which syslog facility channel a record is filed
/// under. It combines with the severity to form the record's priority
/// (`PRI = facility * 8 + severity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Facility {
    /// Kernel messages.
    Kernel = 0,
    /// User-level messages (the default).
    User = 1,
    /// Mail system.
    Mail = 2,
    /// System daemons.
    Daemon = 3,
    /// Security / authorization messages.
    Auth = 4,
    /// Messages generated internally by syslogd.
    Syslog = 5,
    /// Line printer subsystem.
    Lpr = 6,
    /// Network news subsystem.
    News = 7,
    /// UUCP subsystem.
    Uucp = 8,
    /// Clock daemon (cron/at).
    Cron = 9,
    /// Security / authorization messages (private).
    AuthPriv = 10,
    /// FTP daemon.
    Ftp = 11,
    /// Reserved for local use.
    Local0 = 16,
    /// Reserved for local use.
    Local1 = 17,
    /// Reserved for local use.
    Local2 = 18,
    /// Reserved for local use.
    Local3 = 19,
    /// Reserved for local use.
    Local4 = 20,
    /// Reserved for local use.
    Local5 = 21,
    /// Reserved for local use.
    Local6 = 22,
    /// Reserved for local use.
    Local7 = 23,
}

impl Facility {
    /// The numeric facility code.
    #[must_use]
    pub const fn code(self) -> u8 {
        self as u8
    }
}

/// A sink that delivers log records to a syslog daemon.
///
/// The sink connects lazily to the first reachable Unix datagram socket in its
/// configured path list and reconnects automatically if the daemon restarts
/// (a failed send triggers one reconnect-and-retry attempt). Records are
/// formatted as RFC 3164 datagrams:
///
/// ```text
/// <PRI>TIMESTAMP HOSTNAME TAG[PID]: MSG
/// ```
///
/// where `PRI = facility * 8 + severity` is derived from the entry's level.
/// The message body is the record as formatted by [`crate::Logger`], truncated
/// to fit a legacy 1024-byte datagram.
///
/// The writer thread uses [`Sink::write_entry`], so each record's level is
/// encoded accurately. Using [`Sink::write_bytes`] directly assumes the
/// `Info` severity.
pub struct SyslogSink {
    paths: Vec<PathBuf>,
    facility: Facility,
    ident: String,
    hostname: String,
    pid: u32,
    socket: Option<UnixDatagram>,
}

impl SyslogSink {
    /// Create a sink that delivers to `paths`, trying each in order.
    #[must_use]
    pub fn new(
        paths: impl IntoIterator<Item = impl AsRef<Path>>,
        facility: Facility,
        ident: impl Into<String>,
    ) -> SyslogSink {
        SyslogSink {
            paths: paths
                .into_iter()
                .map(|p| p.as_ref().to_path_buf())
                .collect(),
            facility,
            ident: sanitize_ident(ident.into()),
            hostname: read_hostname(),
            pid: std::process::id(),
            socket: None,
        }
    }

    /// Create a sink that delivers to the standard Linux syslog sockets
    /// ([`DEFAULT_SYSLOG_SOCKETS`]).
    #[must_use]
    pub fn local(facility: Facility, ident: impl Into<String>) -> SyslogSink {
        SyslogSink::new(DEFAULT_SYSLOG_SOCKETS, facility, ident)
    }

    /// Deliver a formatted record as a syslog datagram.
    fn deliver(&mut self, level: Level, timestamp: Timestamp, formatted: &[u8]) -> io::Result<()> {
        if self.socket.is_none() {
            self.reconnect()?;
        }
        let packet = self.build_packet(level, timestamp, formatted);
        if let Some(sock) = &self.socket {
            match sock.send(&packet) {
                Ok(_) => Ok(()),
                Err(first) => {
                    // The daemon may have restarted; reconnect once and retry.
                    if self.reconnect().is_ok() {
                        if let Some(sock) = &self.socket {
                            return sock.send(&packet).map(|_| ());
                        }
                    }
                    Err(first)
                }
            }
        } else {
            Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "no syslog socket available",
            ))
        }
    }

    /// Establish a connection to the first reachable socket path.
    fn reconnect(&mut self) -> io::Result<()> {
        for path in &self.paths {
            if let Ok(sock) = UnixDatagram::unbound() {
                if sock.connect(path).is_ok() {
                    self.socket = Some(sock);
                    return Ok(());
                }
            }
        }
        self.socket = None;
        Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "no reachable syslog socket",
        ))
    }

    /// Build an RFC 3164 datagram from a formatted record.
    fn build_packet(&self, level: Level, timestamp: Timestamp, formatted: &[u8]) -> Vec<u8> {
        let pri = self.facility.code() * 8 + level_to_severity(level);
        let header = format!(
            "<{pri}>{} {} {}[{}]: ",
            rfc3164_timestamp(timestamp),
            self.hostname,
            self.ident,
            self.pid
        );
        // Strip the trailing newline added by the record formatter.
        let mut msg = formatted;
        if msg.last() == Some(&b'\n') {
            msg = &msg[..msg.len() - 1];
        }
        let budget = MAX_SYSLOG_PACKET.saturating_sub(header.len());
        let msg = &msg[..msg.len().min(budget)];

        let mut out = String::with_capacity(header.len() + msg.len());
        out.push_str(&header);
        out.push_str(&String::from_utf8_lossy(msg));
        out.into_bytes()
    }
}

impl Sink for SyslogSink {
    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        // Raw byte writes carry no level; assume Info severity.
        self.deliver(Level::Info, Timestamp::now(0), bytes)
    }

    fn write_entry(&mut self, entry: &LogEntry, formatted: &[u8]) -> io::Result<()> {
        self.deliver(entry.level, entry.timestamp, formatted)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Map an [`Level`] to the RFC 3164 severity code.
fn level_to_severity(level: Level) -> u8 {
    match level {
        Level::Trace | Level::Debug => SEVERITY_DEBUG,
        Level::Info => SEVERITY_INFO,
        Level::Warn => SEVERITY_WARNING,
        Level::Error => SEVERITY_ERROR,
        // Unreachable in practice: `Off` records are filtered before they
        // reach the writer thread.
        Level::Off => SEVERITY_DEBUG,
    }
}

/// RFC 3164 month abbreviations.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Format a timestamp as `MMM dd HH:MM:SS` (RFC 3164, day is space padded).
fn rfc3164_timestamp(ts: Timestamp) -> String {
    let month = MONTHS
        .get(usize::from(ts.month.saturating_sub(1)))
        .copied()
        .unwrap_or("Jan");
    format!(
        "{month} {:2} {:02}:{:02}:{:02}",
        ts.day, ts.hour, ts.minute, ts.second
    )
}

/// Restrict a program identity to characters that are safe in an RFC 3164
/// tag: ASCII alphanumerics, `_`, `-` and `.`. Everything else becomes `_`.
fn sanitize_ident(ident: String) -> String {
    if ident.is_empty() {
        return "xoslog".to_string();
    }
    ident
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The hostname reported in the datagram header.
fn read_hostname() -> String {
    if let Ok(content) = std::fs::read_to_string("/etc/hostname") {
        let host = content.trim();
        if !host.is_empty() {
            return host.to_string();
        }
    }
    if let Ok(host) = std::env::var("HOSTNAME") {
        if !host.is_empty() {
            return host;
        }
    }
    "localhost".to_string()
}

/// The default program identity, derived from `argv[0]`.
pub(crate) fn default_ident() -> String {
    std::env::args()
        .next()
        .as_deref()
        .and_then(|arg| {
            std::path::Path::new(arg)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "xoslog".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facility_codes() {
        assert_eq!(Facility::Kernel.code(), 0);
        assert_eq!(Facility::User.code(), 1);
        assert_eq!(Facility::Mail.code(), 2);
        assert_eq!(Facility::Daemon.code(), 3);
        assert_eq!(Facility::Local0.code(), 16);
        assert_eq!(Facility::Local7.code(), 23);
    }

    #[test]
    fn severity_mapping() {
        assert_eq!(level_to_severity(Level::Trace), 7);
        assert_eq!(level_to_severity(Level::Debug), 7);
        assert_eq!(level_to_severity(Level::Info), 6);
        assert_eq!(level_to_severity(Level::Warn), 4);
        assert_eq!(level_to_severity(Level::Error), 3);
    }

    #[test]
    fn rfc3164_timestamp_format() {
        let ts = Timestamp {
            year: 2026,
            month: 8,
            day: 3,
            hour: 4,
            minute: 49,
            second: 5,
            microsecond: 0,
            offset_seconds: 0,
        };
        assert_eq!(rfc3164_timestamp(ts), "Aug  3 04:49:05");
    }

    #[test]
    fn pri_and_header() {
        let sink = SyslogSink::new(Vec::<PathBuf>::new(), Facility::User, "testapp");
        let ts = Timestamp::now(0);
        let packet = sink.build_packet(Level::Info, ts, b"hello\n");
        let s = String::from_utf8(packet).unwrap();
        assert!(s.starts_with("<14>"), "User(1)*8 + Info(6) = 14, got: {s}");
        assert!(s.contains("testapp["));
        assert!(s.ends_with("hello"));
    }

    #[test]
    fn warn_severity_pri() {
        let sink = SyslogSink::new(Vec::<PathBuf>::new(), Facility::Local0, "app");
        let ts = Timestamp::now(0);
        let packet = sink.build_packet(Level::Warn, ts, b"boom");
        assert!(String::from_utf8(packet).unwrap().starts_with("<132>"));
    }

    #[test]
    fn ident_sanitized() {
        let sink = SyslogSink::new(Vec::<PathBuf>::new(), Facility::User, "my app#1");
        assert_eq!(sink.ident, "my_app_1");
    }

    #[test]
    fn oversized_message_truncated() {
        let sink = SyslogSink::new(Vec::<PathBuf>::new(), Facility::Daemon, "t");
        let ts = Timestamp::now(0);
        let big = vec![b'x'; 5000];
        let packet = sink.build_packet(Level::Error, ts, &big);
        assert!(packet.len() <= MAX_SYSLOG_PACKET);
    }
}
