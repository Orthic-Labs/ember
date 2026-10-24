//! Mobile startup must install the engine's reporter without depending on a desktop main.
//! A child process keeps the panic hook and crash configuration isolated from other tests.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use lightcraft_mobile::{EmberSession, SessionOptions};
use serde_json::Value;

const CHILD_ENV: &str = "EMBER_MOBILE_PANIC_REPORT_TEST_CHILD";
const PRIVATE_TEXT: &str = "private mobile library title";
const EMAIL: &str = "mobile-panic-test@example.org";

#[test]
fn session_startup_reports_contained_panics() {
    if std::env::var_os(CHILD_ENV).is_some() {
        exercise_mobile_startup();
        return;
    }

    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos();
    let config_dir = std::env::temp_dir().join(format!("ember-mobile-panic-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&config_dir).expect("create isolated profile");
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", "session_startup_reports_contained_panics", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env("LIGHTCRAFT_CONFIG_DIR", &config_dir)
        .env("RUST_BACKTRACE", "0")
        .output()
        .expect("run isolated mobile startup");
    assert!(output.status.success(), "child failed: {}", String::from_utf8_lossy(&output.stderr));

    assert_reports(&config_dir.join("lightcraft-crash-reports"));
    std::fs::remove_dir_all(config_dir).expect("remove isolated profile");
}

fn exercise_mobile_startup() {
    let open = || EmberSession::open(SessionOptions { library_path: None, demo: true, demo_count: 3 }).expect("session opens");
    let session = open();
    let before: Value = serde_json::from_str(&session.snapshot().expect("snapshot before panic")).expect("snapshot JSON");
    // Repeated opens use the existing guard's one-time initialization, rather than chaining hooks.
    let second = open();
    let error = lightcraft_engine::guard::catch("mobile reporting regression", || {
        panic!("failed on `{PRIVATE_TEXT}` for {EMAIL}");
    })
    .expect_err("guard contains panic");
    assert!(error.starts_with("mobile reporting regression failed unexpectedly:"));
    let after: Value = serde_json::from_str(&session.snapshot().expect("same session survives panic")).expect("snapshot JSON");
    assert!(after.is_object());
    assert_eq!(after["viewGeneration"], before["viewGeneration"]);
    second.close().expect("close second session");
    session.close().expect("close original session");
}

fn assert_reports(crash_dir: &Path) {
    let reports: Vec<Value> = std::fs::read_dir(crash_dir)
        .expect("mobile startup installed crash reporting")
        .map(|entry| entry.expect("report entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "json"))
        .map(|path| {
            let text = std::fs::read_to_string(path).expect("read report");
            assert!(!text.contains(PRIVATE_TEXT), "quoted app content must be redacted");
            assert!(!text.contains(EMAIL), "email must be redacted");
            serde_json::from_str(&text).expect("report JSON")
        })
        .collect();
    assert_eq!(reports.len(), 2, "one hook report and one contained-panic report after repeated opens");
    for kind in ["panic", "guard_caught"] {
        let report = reports.iter().find(|report| report["kind"] == kind).expect("expected report kind");
        assert_eq!(report["app_name"], "Ember");
        assert_eq!(report["process_kind"], "engine");
        assert!(report["message"].as_str().expect("report message").contains("redacted"));
    }
    let caught = reports.iter().find(|report| report["kind"] == "guard_caught").expect("contained-panic report");
    assert!(caught["message"].as_str().expect("caught message").starts_with("mobile reporting regression failed unexpectedly:"));
}
