//! Issue #44: Assisted Culling as a reviewed proposal. The analysis is queued off the UI thread,
//! nothing is flagged before Apply, Apply sets exactly the reviewed flags in one undo step, and
//! cancelling leaves the catalog untouched.

use std::time::{Duration, Instant};

use lightcraft_catalog::Flag;
use lightcraft_engine::Session;

use crate::cull_review::{self, Phase};
use crate::headless::Headless;
use crate::{LightcraftApp, Services};

fn app() -> LightcraftApp {
    LightcraftApp::new(Session::with_demo(), Services { png: None, ..Default::default() })
}

/// Every photo's id and flag, in catalog order.
fn flags(app: &LightcraftApp) -> Vec<(u64, Flag)> {
    app.session.catalog.photos().map(|photo| (photo.id.0, photo.flag)).collect()
}

/// Polls until no job is running; returns the notice the last poll raised, if any.
fn settle(app: &mut LightcraftApp) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut notice = None;
    while matches!(app.cull_review.phase, Phase::Analyzing(_) | Phase::Applying(_)) && Instant::now() < deadline {
        notice = cull_review::poll_state(app).or(notice);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!matches!(app.cull_review.phase, Phase::Analyzing(_) | Phase::Applying(_)), "the job finished in time");
    notice
}

/// The (photo, suggested flag) pairs the review offers.
fn suggested(app: &LightcraftApp) -> Vec<(u64, String)> {
    match &app.cull_review.phase {
        Phase::Review(review) => review.rows.iter().filter_map(|row| row.proposed.clone().map(|flag| (row.id, flag))).collect(),
        _ => Vec::new(),
    }
}

#[test]
fn analysis_is_queued_as_a_background_job() {
    let mut app = app();
    let before = flags(&app);
    let undo = app.session.undo.len();
    cull_review::start(&mut app, 100.0, true).expect("analysis starts");
    assert!(matches!(app.cull_review.phase, Phase::Analyzing(_)), "the call returns with the analysis queued");
    assert!(app.session.activity.list().iter().any(|task| task.kind == "cull"), "the analysis has its own activity row");
    assert_eq!(flags(&app), before, "nothing is flagged while the analysis runs");
    assert_eq!(app.session.undo.len(), undo, "the analysis adds no undo step");
    assert_eq!(settle(&mut app), None);
    assert!(matches!(app.cull_review.phase, Phase::Review(_)), "the finished analysis is a proposal to review");
}

#[test]
fn no_flag_changes_before_apply() {
    let mut app = app();
    let before = flags(&app);
    cull_review::start(&mut app, 100.0, true).expect("analysis starts");
    assert_eq!(settle(&mut app), None);
    assert!(!suggested(&app).is_empty(), "a reject score of 100 suggests rejects");
    assert_eq!(flags(&app), before, "the proposal is not applied by reviewing it");
}

#[test]
fn apply_sets_exactly_the_reviewed_flags_in_one_undo_step() {
    let mut app = app();
    cull_review::start(&mut app, 100.0, true).expect("analysis starts");
    assert_eq!(settle(&mut app), None);
    let offered = suggested(&app);
    assert!(offered.len() >= 2, "the demo library offers at least two suggestions");
    let (kept_id, kept_flag) = offered[0].clone();
    let expected_flag = if kept_flag == "pick" { Flag::Pick } else { Flag::Reject };
    if let Phase::Review(review) = &mut app.cull_review.phase {
        for row in &mut review.rows {
            row.accept = row.id == kept_id;
        }
    }
    let before = flags(&app);
    let undo = app.session.undo.len();
    cull_review::apply(&mut app).expect("apply starts");
    assert_eq!(settle(&mut app), None);
    assert_eq!(app.session.undo.len(), undo + 1, "one undo step for the whole apply");
    for ((id, before_flag), (_, after_flag)) in before.iter().zip(flags(&app)) {
        let expected = if *id == kept_id { expected_flag } else { *before_flag };
        assert_eq!(after_flag, expected, "photo {id}");
    }
}

#[test]
fn cancel_leaves_the_catalog_untouched() {
    let mut app = app();
    let before = flags(&app);
    let undo = app.session.undo.len();
    cull_review::start(&mut app, 100.0, true).expect("analysis starts");
    cull_review::cancel(&mut app);
    assert_eq!(settle(&mut app), None, "a cancelled analysis raises no notice");
    assert!(matches!(app.cull_review.phase, Phase::Idle), "a cancelled analysis shows no proposal");
    assert_eq!(flags(&app), before, "cancelling flags nothing");
    assert_eq!(app.session.undo.len(), undo, "cancelling adds no undo step");
}

/// The import dialog's Assisted Culling option (#41): the import queues the read-only pass for exactly the photos it added.
#[test]
fn import_with_the_culling_option_queues_the_pass_for_the_imported_photos() {
    let dir = std::env::temp_dir().join(format!("lc-import-cull-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..3 {
        std::fs::write(dir.join(format!("IMG_{i:03}.jpg")), format!("not really a jpeg {i}")).unwrap();
    }
    let mut session = Session::new();
    session.media.file_probe = Some(std::sync::Arc::new(|p: &str| {
        Ok(lightcraft_engine::media::ProbeInfo {
            width: 60,
            height: 40,
            format: "JPEG".into(),
            content_hash: Some(p.to_string()),
            ..Default::default()
        })
    }));
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0);
    h.settle(Duration::from_secs(20));
    let undo0 = h.app.session.undo.len();
    let task = crate::import::ImportTask::new(vec![dir.to_string_lossy().to_string()], serde_json::json!({"mode": "add", "cull": true}), undo0, false);
    h.app.import = Some(task);
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.import.is_none()));
    let mut added: Vec<u64> = h.app.session.catalog.photos().map(|photo| photo.id.0).collect();
    added.sort_unstable();
    assert_eq!(added.len(), 3, "all imported");
    assert_eq!(h.app.cull_review.import_cull_queued.len(), 1, "one pass queued");
    let mut queued = h.app.cull_review.import_cull_queued[0].clone();
    queued.sort_unstable();
    assert_eq!(queued, added, "the pass covers exactly the imported photos");
    let _ = std::fs::remove_dir_all(&dir);
}
