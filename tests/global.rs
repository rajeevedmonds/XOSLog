use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use xoslog::{log_debug, log_error, log_info, log_trace, log_warn, Level, LoggerBuilder};

fn temp_file(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "xoslog-global-{tag}-{}-{nanos}",
        std::process::id()
    ))
}

/// The process-wide global logger can only be installed once, and tests in
/// this binary run in parallel, so everything that touches it lives in a
/// single test guarded by a mutex.
#[test]
fn global_macros() {
    // Fresh process for this test binary => no global logger yet.
    assert!(xoslog::global().is_none());

    // Without a global logger the macros expand to a no-op.
    log_info!("before install");
    log_error!("before install");

    let path = temp_file("macros");
    let logger = LoggerBuilder::new()
        .include_location(false)
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    assert!(xoslog::set_global(logger).is_ok());
    assert!(xoslog::global().is_some());

    log_trace!("trace filtered by default Info threshold");
    log_debug!("debug filtered by default Info threshold");
    log_info!("started {}", "app");
    log_warn!("low disk space");
    log_error!("fatal error code {}", 7);
    xoslog::log!(Level::Info, "explicit level {}", 1);

    xoslog::global().unwrap().flush();

    let content = std::fs::read_to_string(&path).unwrap();

    // Default threshold is Info, so Trace/Debug are filtered out.
    assert!(!content.contains("trace filtered"));
    assert!(!content.contains("debug filtered"));

    assert!(content.contains(" [INFO] started app"));
    assert!(content.contains(" [WARN] low disk space"));
    assert!(content.contains(" [ERROR] fatal error code 7"));
    assert!(content.contains(" [INFO] explicit level 1"));
}
