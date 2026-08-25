use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use xoslog::{log_debug, log_info, log_trace, Level, LogEntry, LoggerBuilder, TargetFilter};

fn temp_file(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("xoslog-filter-{tag}-{}-{nanos}", std::process::id()))
}

/// Tests that read the process-wide `XOSLOG` variable or install the global
/// logger must not run concurrently with one another.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn record(target: &'static str, level: Level, message: &str) -> LogEntry {
    LogEntry::new(level, message.to_string(), target, "tests/filter.rs", 0)
}

fn contents(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn per_target_levels_apply_at_log_time() {
    let path = temp_file("targets");
    let filter = TargetFilter::parse("myapp=debug,hyper=warn");
    let logger = LoggerBuilder::new()
        .level(Level::Info)
        .filter(filter)
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.log(record("myapp", Level::Debug, "myapp debug ok"));
    logger.log(record("myapp::server", Level::Debug, "descendant debug ok"));
    logger.log(record("hyper", Level::Info, "hyper info filtered"));
    logger.log(record("hyper", Level::Warn, "hyper warn ok"));
    logger.log(record("unrelated", Level::Debug, "unrelated debug filtered"));
    logger.log(record("unrelated", Level::Info, "unrelated info ok"));
    logger.flush();

    let content = contents(&path);
    assert!(content.contains("myapp debug ok"), "{content}");
    assert!(content.contains("descendant debug ok"), "{content}");
    assert!(!content.contains("hyper info filtered"), "{content}");
    assert!(content.contains("hyper warn ok"), "{content}");
    assert!(!content.contains("unrelated debug filtered"), "{content}");
    assert!(content.contains("unrelated info ok"), "{content}");
}

#[test]
fn longest_match_and_module_boundary() {
    let path = temp_file("longest");
    let logger = LoggerBuilder::new()
        .level(Level::Info)
        .filter(TargetFilter::parse("lib=info,lib::net=debug"))
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.log(record("lib", Level::Info, "lib info ok"));
    logger.log(record("lib::net", Level::Debug, "lib::net debug ok"));
    logger.log(record("lib::net::tls", Level::Debug, "deep debug ok"));
    logger.log(record("lib::net", Level::Info, "lib::net info ok"));
    logger.log(record("lib::netx", Level::Debug, "sibling must NOT get debug"));
    logger.log(record("lib::netx", Level::Info, "sibling info ok"));
    logger.flush();

    let content = contents(&path);
    assert!(content.contains("lib info ok"), "{content}");
    assert!(content.contains("lib::net debug ok"), "{content}");
    assert!(content.contains("deep debug ok"), "{content}");
    assert!(content.contains("lib::net info ok"), "{content}");
    assert!(!content.contains("sibling must NOT get debug"), "{content}");
    assert!(content.contains("sibling info ok"), "{content}");
}

#[test]
fn off_suppresses_only_that_target() {
    let path = temp_file("off");
    let logger = LoggerBuilder::new()
        .level(Level::Info)
        .filter(TargetFilter::parse("myapp=off"))
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.log(record("myapp", Level::Error, "myapp error silenced"));
    logger.log(record("other", Level::Error, "other error ok"));
    logger.log(record("other", Level::Info, "other info ok"));
    logger.flush();

    let content = contents(&path);
    assert!(!content.contains("myapp error silenced"), "{content}");
    assert!(content.contains("other error ok"), "{content}");
    assert!(content.contains("other info ok"), "{content}");
}

#[test]
fn global_directive_overrides_base_level() {
    let path = temp_file("global");
    let logger = LoggerBuilder::new()
        .level(Level::Info)
        .filter(TargetFilter::parse("debug"))
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.log(record("any", Level::Debug, "global debug ok"));
    logger.log(record("any", Level::Trace, "trace still filtered"));
    logger.flush();

    let content = contents(&path);
    assert!(content.contains("global debug ok"), "{content}");
    assert!(!content.contains("trace still filtered"), "{content}");
}

#[test]
fn bare_target_enables_verbose_logging() {
    let path = temp_file("bare");
    let logger = LoggerBuilder::new()
        .level(Level::Info)
        .filter(TargetFilter::parse("hyper"))
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.log(record("hyper", Level::Trace, "hyper trace ok"));
    logger.log(record("other", Level::Trace, "other trace filtered"));
    logger.log(record("other", Level::Info, "other info ok"));
    logger.flush();

    let content = contents(&path);
    assert!(content.contains("hyper trace ok"), "{content}");
    assert!(!content.contains("other trace filtered"), "{content}");
    assert!(content.contains("other info ok"), "{content}");
}

#[test]
fn empty_filter_leaves_base_level_untouched() {
    let path = temp_file("empty");
    let logger = LoggerBuilder::new()
        .level(Level::Warn)
        .filter(TargetFilter::default())
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.log(record("a", Level::Info, "info filtered"));
    logger.log(record("a", Level::Warn, "warn ok"));
    logger.flush();

    let content = contents(&path);
    assert!(!content.contains("info filtered"), "{content}");
    assert!(content.contains("warn ok"), "{content}");
}

#[test]
fn is_enabled_for_reflects_filter() {
    let logger = LoggerBuilder::new()
        .level(Level::Info)
        .filter(TargetFilter::parse("myapp=debug,hyper=off"))
        .to_stderr()
        .build()
        .unwrap();

    assert!(logger.is_enabled_for(Level::Debug, "myapp"));
    assert!(logger.is_enabled_for(Level::Debug, "myapp::sub"));
    assert!(!logger.is_enabled_for(Level::Trace, "myapp"));
    assert!(!logger.is_enabled_for(Level::Error, "hyper"));
    assert!(logger.is_enabled_for(Level::Info, "unlisted"));
    assert!(!logger.is_enabled_for(Level::Debug, "unlisted"));

    // The default (unmatched) behaviour is unchanged.
    assert!(logger.is_enabled(Level::Info));
    assert!(!logger.is_enabled(Level::Debug));
    assert_eq!(logger.filter().effective_level("myapp", Level::Info), Level::Debug);
    logger.shutdown();
}

mod inner {
    // Macro calls in this module resolve to the target `filter::inner`.
    pub(super) fn emit_debug() {
        xoslog::log_debug!("inner debug ok");
    }

    pub(super) fn emit_info() {
        xoslog::log_info!("inner info ok");
    }
}

#[test]
fn macros_use_module_path_as_target() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let path = temp_file("macros");
    let logger = LoggerBuilder::new()
        .level(Level::Info)
        .filter(TargetFilter::parse("filter::inner=debug"))
        .include_location(false)
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();
    let _ = xoslog::set_global(logger);

    log_trace!("top-level trace filtered");
    log_debug!("top-level debug filtered");
    log_info!("top-level info ok");
    inner::emit_debug();
    inner::emit_info();

    xoslog::global().unwrap().flush();

    let content = contents(&path);
    assert!(!content.contains("top-level trace filtered"), "{content}");
    assert!(!content.contains("top-level debug filtered"), "{content}");
    assert!(content.contains("top-level info ok"), "{content}");
    assert!(content.contains("inner debug ok"), "{content}");
    assert!(content.contains("inner info ok"), "{content}");
}

#[test]
fn env_var_controls_filter_by_default() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    std::env::set_var("XOSLOG", "envmod=warn");
    let path = temp_file("env");
    let logger = LoggerBuilder::new()
        .level(Level::Info)
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.log(record("envmod", Level::Info, "envmod info filtered"));
    logger.log(record("envmod", Level::Warn, "envmod warn ok"));
    logger.log(record("other", Level::Info, "other info ok"));
    logger.flush();

    let content = contents(&path);
    assert!(!content.contains("envmod info filtered"), "{content}");
    assert!(content.contains("envmod warn ok"), "{content}");
    assert!(content.contains("other info ok"), "{content}");

    std::env::remove_var("XOSLOG");
}

#[test]
fn env_var_ignored_when_explicit_filter_given() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    std::env::set_var("XOSLOG", "envmod=off");
    let path = temp_file("explicit");
    let logger = LoggerBuilder::new()
        .level(Level::Info)
        .filter(TargetFilter::parse("envmod=warn"))
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.log(record("envmod", Level::Warn, "envmod warn ok"));
    logger.flush();

    let content = contents(&path);
    assert!(content.contains("envmod warn ok"), "{content}");

    std::env::remove_var("XOSLOG");
}

#[test]
fn ignore_env_filter_disables_env_reading() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    std::env::set_var("XOSLOG", "envmod=off");
    let path = temp_file("ignore");
    let logger = LoggerBuilder::new()
        .level(Level::Info)
        .ignore_env_filter()
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.log(record("envmod", Level::Info, "envmod info ok despite env"));
    logger.flush();

    let content = contents(&path);
    assert!(content.contains("envmod info ok despite env"), "{content}");

    std::env::remove_var("XOSLOG");
}

#[test]
fn empty_env_var_is_ignored() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    std::env::set_var("XOSLOG", "   ");
    let path = temp_file("blankenv");
    let logger = LoggerBuilder::new()
        .level(Level::Warn)
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.log(record("anything", Level::Info, "info filtered"));
    logger.log(record("anything", Level::Warn, "warn ok"));
    logger.flush();

    let content = contents(&path);
    assert!(!content.contains("info filtered"), "{content}");
    assert!(content.contains("warn ok"), "{content}");

    std::env::remove_var("XOSLOG");
}

#[test]
fn invalid_env_spec_is_lenient() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    std::env::set_var("XOSLOG", "broken=notalevel,envmod=warn");
    let path = temp_file("lenient");
    let logger = LoggerBuilder::new()
        .level(Level::Info)
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.log(record("broken", Level::Info, "broken target keeps base"));
    logger.log(record("envmod", Level::Info, "envmod info filtered"));
    logger.log(record("envmod", Level::Warn, "envmod warn ok"));
    logger.flush();

    let content = contents(&path);
    assert!(content.contains("broken target keeps base"), "{content}");
    assert!(!content.contains("envmod info filtered"), "{content}");
    assert!(content.contains("envmod warn ok"), "{content}");

    std::env::remove_var("XOSLOG");
}
