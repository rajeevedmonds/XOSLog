use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use xoslog::LoggerBuilder;

fn temp_file(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "xoslog-rotation-{tag}-{}-{nanos}",
        std::process::id()
    ))
}

fn exists(path: &Path) -> bool {
    path.metadata().is_ok()
}

#[test]
fn rotation_keeps_bounded_number_of_backups() {
    let path = temp_file("bounded");
    let logger = LoggerBuilder::new()
        .include_location(false)
        .to_file(&path, 300, 3)
        .unwrap()
        .build()
        .unwrap();

    for i in 0..100 {
        logger.info(format!(
            "message number {i} with some padding to force rotation"
        ));
    }
    logger.flush();

    assert!(exists(&path));
    assert!(exists(Path::new(&format!("{}.1", path.display()))));
    assert!(exists(Path::new(&format!("{}.2", path.display()))));
    assert!(exists(Path::new(&format!("{}.3", path.display()))));
    assert!(
        !exists(Path::new(&format!("{}.4", path.display()))),
        "older backups should have been pruned"
    );

    // The active file must not exceed the rotation threshold.
    let size = std::fs::metadata(&path).unwrap().len();
    assert!(size <= 300, "active file grew past max_size: {size}");
}

#[test]
fn rotation_truncates_in_place_when_no_backups() {
    let path = temp_file("truncate");
    let logger = LoggerBuilder::new()
        .include_location(false)
        .to_file(&path, 200, 0)
        .unwrap()
        .build()
        .unwrap();

    for i in 0..100 {
        logger.info(format!("message {i} - padding padding padding"));
    }
    logger.flush();

    assert!(!exists(Path::new(&format!("{}.1", path.display()))));
    let size = std::fs::metadata(&path).unwrap().len();
    assert!(size <= 200, "active file should be truncated, got {size}");
}

#[test]
fn no_rotation_when_max_size_is_zero() {
    let path = temp_file("norotate");
    let logger = LoggerBuilder::new()
        .include_location(false)
        .to_file(&path, 0, 3)
        .unwrap()
        .build()
        .unwrap();

    for i in 0..50 {
        logger.info(format!("message {i}"));
    }
    logger.flush();

    assert!(
        !exists(Path::new(&format!("{}.1", path.display()))),
        "no rotation expected"
    );
}

#[test]
fn time_rotation_every_second_creates_a_backup() {
    use std::thread;
    use std::time::Duration;
    use xoslog::RotationInterval;

    let path = temp_file("interval");
    let logger = LoggerBuilder::new()
        .include_location(false)
        .to_file_with_interval(&path, 0, 2, RotationInterval::Every(Duration::from_secs(1)))
        .unwrap()
        .build()
        .unwrap();

    logger.info("before interval");
    logger.flush();
    assert!(
        !exists(Path::new(&format!("{}.1", path.display()))),
        "must not rotate before the interval elapses"
    );

    thread::sleep(Duration::from_millis(1_100));
    logger.info("after interval");
    logger.flush();

    assert!(
        exists(Path::new(&format!("{}.1", path.display()))),
        "expected a backup after the interval elapsed"
    );
    let active = std::fs::read_to_string(&path).unwrap();
    assert!(
        active.contains("after interval"),
        "active file should hold the post-rotation record, got: {active:?}"
    );
}

#[test]
fn daily_interval_does_not_rotate_immediately() {
    use xoslog::RotationInterval;

    let path = temp_file("daily");
    let logger = LoggerBuilder::new()
        .include_location(false)
        .to_file_with_interval(&path, 0, 3, RotationInterval::Daily)
        .unwrap()
        .build()
        .unwrap();

    for i in 0..10 {
        logger.info(format!("same-day message {i}"));
    }
    logger.flush();

    assert!(
        !exists(Path::new(&format!("{}.1", path.display()))),
        "daily rotation must not fire within the same UTC day"
    );
}
