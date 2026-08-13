use std::net::UdpSocket;
use std::time::Duration;

use xoslog::{Facility, LoggerBuilder, RemoteSyslogSink};

/// A fake remote syslog server listening on a UDP socket.
fn fake_server() -> (UdpSocket, std::net::SocketAddr) {
    let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
    listener
        .set_read_timeout(Some(Duration::from_millis(2000)))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}

fn recv_packet(listener: &UdpSocket) -> String {
    let mut buf = [0u8; 2048];
    let (n, _) = listener.recv_from(&mut buf).unwrap();
    String::from_utf8_lossy(&buf[..n]).to_string()
}

#[test]
fn delivers_datagrams_to_remote_udp_server() {
    let (listener, addr) = fake_server();

    let sink = RemoteSyslogSink::new(addr, Facility::Local1, "remote-test").unwrap();
    let logger = LoggerBuilder::new()
        .level(xoslog::Level::Info)
        .include_location(false)
        .sink(sink)
        .build()
        .unwrap();

    logger.info("hello remote");
    logger.error("broken pipe");
    logger.flush();

    let first = recv_packet(&listener);
    // Local1 = 17, Info = 6 => PRI 142.
    assert!(
        first.starts_with("<142>"),
        "expected PRI <142>, got: {first}"
    );
    assert!(first.contains("remote-test["), "missing ident: {first}");
    assert!(
        first.ends_with("hello remote"),
        "unexpected message: {first}"
    );

    let second = recv_packet(&listener);
    // Local1 = 17, Error = 3 => PRI 139.
    assert!(
        second.starts_with("<139>"),
        "expected PRI <139>, got: {second}"
    );
    assert!(
        second.ends_with("broken pipe"),
        "unexpected message: {second}"
    );
}

#[test]
fn builder_resolves_host_and_port() {
    let (listener, addr) = fake_server();
    let port = addr.port();

    let logger = LoggerBuilder::new()
        .level(xoslog::Level::Warn)
        .include_location(false)
        .to_remote_syslog("127.0.0.1", port, Facility::Daemon)
        .unwrap()
        .build()
        .unwrap();

    logger.warn("via builder");
    logger.flush();

    let msg = recv_packet(&listener);
    // Daemon = 3, Warn = 4 => PRI 28.
    assert!(msg.starts_with("<28>"), "expected PRI <28>, got: {msg}");
    assert!(msg.ends_with("via builder"), "unexpected message: {msg}");
}

#[test]
fn records_survive_when_server_unreachable() {
    // Bind a socket, grab its port, then drop it so nothing is listening.
    let (probe, addr) = fake_server();
    let port = addr.port();
    drop(probe);

    let sink = RemoteSyslogSink::new(addr, Facility::User, "unreachable").unwrap();
    let logger = LoggerBuilder::new()
        .include_location(false)
        .sink(sink)
        .build()
        .unwrap();

    // UDP is fire-and-forget: logging must never block or panic.
    for i in 0..50 {
        logger.info(format!("message {i} to nowhere"));
    }
    logger.flush();
    let _ = port;
}

#[test]
fn message_truncated_to_legacy_size() {
    let (listener, addr) = fake_server();

    let sink = RemoteSyslogSink::new(addr, Facility::Local0, "trunc").unwrap();
    let logger = LoggerBuilder::new()
        .include_location(false)
        .sink(sink)
        .build()
        .unwrap();

    let big = "x".repeat(5000);
    logger.info(big);
    logger.flush();

    let msg = recv_packet(&listener);
    assert!(msg.len() <= 1024, "datagram too large: {}", msg.len());
}
