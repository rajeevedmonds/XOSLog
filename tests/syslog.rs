use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use xoslog::{Facility, LoggerBuilder, SyslogSink};

fn temp_socket(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "xoslog-syslog-{tag}-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn delivers_datagrams_to_syslog_socket() {
    let path = temp_socket("basic");
    let listener = UnixDatagram::bind(&path).unwrap();
    listener
        .set_read_timeout(Some(Duration::from_millis(2000)))
        .unwrap();

    let sink = SyslogSink::new([path.clone()], Facility::Local0, "xoslog-test");
    let logger = LoggerBuilder::new()
        .level(xoslog::Level::Info)
        .include_location(false)
        .sink(sink)
        .build()
        .unwrap();

    logger.info("hello syslog");
    logger.warn("disk low");
    logger.flush();

    let mut buf = [0u8; 2048];
    let (n, _) = listener.recv_from(&mut buf).unwrap();
    let first = String::from_utf8_lossy(&buf[..n]).to_string();
    // Local0 = 16, Info = 6 => PRI 16*8 + 6 = 134.
    assert!(
        first.starts_with("<134>"),
        "expected PRI <134>, got: {first}"
    );
    assert!(first.contains("xoslog-test["), "missing ident: {first}");
    assert!(
        first.contains('Z') || first.contains(": "),
        "missing header: {first}"
    );
    assert!(
        first.ends_with("hello syslog"),
        "unexpected message: {first}"
    );

    let (n, _) = listener.recv_from(&mut buf).unwrap();
    let second = String::from_utf8_lossy(&buf[..n]).to_string();
    // Local0 = 16, Warn = 4 => PRI 16*8 + 4 = 132.
    assert!(
        second.starts_with("<132>"),
        "expected PRI <132>, got: {second}"
    );
    assert!(second.ends_with("disk low"), "unexpected message: {second}");
}

#[test]
fn reconnects_when_daemon_starts_later() {
    let path = temp_socket("late");
    let sink = SyslogSink::new([path.clone()], Facility::Daemon, "reconnect");
    let logger = LoggerBuilder::new()
        .include_location(false)
        .sink(sink)
        .build()
        .unwrap();

    // The daemon is not up yet, but the logger must still build.
    logger.info("before daemon");
    logger.flush();

    // The daemon now appears.
    let listener = UnixDatagram::bind(&path).unwrap();
    listener
        .set_read_timeout(Some(Duration::from_millis(2000)))
        .unwrap();

    logger.info("after daemon");
    logger.flush();

    let mut buf = [0u8; 2048];
    let (n, _) = listener.recv_from(&mut buf).unwrap();
    let msg = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(msg.ends_with("after daemon"), "unexpected message: {msg}");
}

#[test]
fn severity_reflects_record_level() {
    let path = temp_socket("severity");
    let listener = UnixDatagram::bind(&path).unwrap();
    listener
        .set_read_timeout(Some(Duration::from_millis(2000)))
        .unwrap();

    let sink = SyslogSink::new([path.clone()], Facility::User, "sev");
    let logger = LoggerBuilder::new()
        .level(xoslog::Level::Debug)
        .include_location(false)
        .sink(sink)
        .build()
        .unwrap();

    logger.debug("low detail");
    logger.error("bad things");
    logger.flush();

    let mut buf = [0u8; 2048];
    let (n, _) = listener.recv_from(&mut buf).unwrap();
    let first = String::from_utf8_lossy(&buf[..n]).to_string();
    // User = 1, Debug = 7 => PRI 15.
    assert!(first.starts_with("<15>"), "expected <15>, got: {first}");

    let (n, _) = listener.recv_from(&mut buf).unwrap();
    let second = String::from_utf8_lossy(&buf[..n]).to_string();
    // User = 1, Error = 3 => PRI 11.
    assert!(second.starts_with("<11>"), "expected <11>, got: {second}");
}
