//! The core's own log and crash notes (src/logs.rs). One test binary of its own: the log
//! is one per process.

use std::fs;
use std::path::PathBuf;
use std::thread;

use elsewhere::logs;

fn log_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("elsewhere-logs-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

#[test]
fn the_log_waits_for_its_folder_rotates_and_notes_crashes() {
    // Lines written before init are kept and written by init.
    logs::info("core", "before init").write();
    let dir = log_dir();
    logs::init(&dir);
    let recent = logs::recent(10);
    assert!(recent[0].contains(r#""msg":"before init""#), "{recent:?}");

    // One JSON object per line, with what the line carries.
    logs::warn("playlist", "Slow server")
        .job(7)
        .item(2)
        .detail("HTTP Error 503")
        .write();
    let line = logs::recent(1).pop().unwrap();
    assert!(line.starts_with(r#"{"t":""#), "{line}");
    assert!(
        line.ends_with(
            r#""lvl":"warn","area":"playlist","job":7,"item":2,"msg":"Slow server","detail":"HTTP Error 503"}"#
        ),
        "{line}"
    );

    // Past 512 KB the file becomes elsewhere.1.log; together they stay near 1 MB.
    let filler = "x".repeat(1000);
    for n in 0..1200 {
        logs::info("test", format!("{n} {filler}")).write();
    }
    let current = fs::metadata(dir.join("elsewhere.log")).unwrap().len();
    let older = fs::metadata(dir.join("elsewhere.1.log")).unwrap().len();
    assert!(current <= 512 * 1024 + 2048, "{current}");
    assert!(older <= 512 * 1024 + 2048, "{older}");
    let last = logs::recent(1).pop().unwrap();
    assert!(last.contains(r#""msg":"1199 x"#), "the newest line is last");
    assert_eq!(logs::recent(5000).len(), {
        let count = |name: &str| fs::read_to_string(dir.join(name)).unwrap().lines().count();
        count("elsewhere.1.log") + count("elsewhere.log")
    });

    // A panic, even one that is caught, leaves a log line and a crash note.
    assert!(logs::crash_notes().is_empty());
    let crashed = thread::Builder::new()
        .name("worker-test".to_owned())
        .spawn(|| panic!("something broke"))
        .unwrap()
        .join();
    assert!(crashed.is_err());
    let notes = logs::crash_notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    let note = fs::read_to_string(&notes[0]).unwrap();
    assert!(note.contains("thread: worker-test"), "{note}");
    assert!(note.contains("message: something broke"), "{note}");
    assert!(note.contains("where: tests/recovery.rs:"), "{note}");
    assert!(
        note.contains(&format!("version: {}", env!("CARGO_PKG_VERSION"))),
        "{note}"
    );
    let line = logs::recent(1).pop().unwrap();
    assert!(
        line.contains(r#""lvl":"error","area":"core""#) && line.contains("Panic in worker-test"),
        "{line}"
    );

    // Two panics in the same second get two notes.
    for _ in 0..2 {
        let _ = thread::spawn(|| panic!("again")).join();
    }
    assert_eq!(logs::crash_notes().len(), 3);
}
