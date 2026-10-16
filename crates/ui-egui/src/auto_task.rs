//! Auto Settings off the UI thread, for a host that measures elsewhere (#192).
//!
//! Auto is two phases ([`lightcraft_engine::auto_measure`]): measuring the decoded source, which
//! needs no session, and applying the values, which is one undo step on this thread
//! (`develop.applyAutoValues`). A host that has somewhere else to measure (the web's render
//! workers) sets [`LightcraftApp::auto_offload`]; `develop.auto`, `develop.autoTone` and
//! `develop.autoControl` then hand the measure over ([`intercept`]), show a row in the activity
//! stack while it is away, and apply what comes back ([`poll`]). The apply is refused when the
//! photo changed in the meantime. Without a host, or when it declines, the command runs in the
//! session as before, measure and apply together.
//!
//! `dryRun` stays in the session: it has to answer with the values.

use std::rc::Rc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use lightcraft_engine::activity::{Cancel, TaskGuard};
use lightcraft_engine::auto_measure::{AutoInput, auto_input, is_auto_control};
use lightcraft_engine::catalog::PhotoId;
use lightcraft_engine::pipeline::auto::regions::RegionAuto;
use serde_json::{Value, json};

use crate::LightcraftApp;

/// The host's answer: Auto's values, or why they could not be measured. Called once, on the UI thread.
pub type AutoDone = Box<dyn FnOnce(Result<RegionAuto, String>)>;

/// Measures `input` somewhere that doesn't block the UI thread and reports through the callback.
/// `false`: the host can't (no worker is alive); the command then runs in the session.
pub type AutoOffload = Rc<dyn Fn(AutoInput, AutoDone) -> bool>;

/// One measure that is away: what to apply it to, and the row that shows it.
pub struct AutoTask {
    photo: PhotoId,
    basis: String,
    control: Option<String>,
    rx: Receiver<Result<RegionAuto, String>>,
    _row: TaskGuard,
}

impl AutoTask {
    /// The photo being measured.
    pub fn photo(&self) -> PhotoId {
        self.photo
    }
}

/// Hand an Auto command to the host's [`LightcraftApp::auto_offload`]. `None`: not for the host
/// (no host, a dry run, another command, or something the session should refuse in its own
/// words), so the command runs in the session.
pub fn intercept(app: &mut LightcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let offload = app.auto_offload.clone()?;
    let (regions, control) = match id {
        "develop.auto" | "develop.autoTone" => {
            if params.get("dryRun").and_then(Value::as_bool).unwrap_or(false) {
                return None;
            }
            (params.get("regions").and_then(Value::as_bool).unwrap_or(true), None)
        }
        "develop.autoControl" => {
            let control = params.get("control").and_then(Value::as_str).filter(|c| is_auto_control(c))?;
            (false, Some(control.to_string()))
        }
        _ => return None,
    };
    let photo = app.session.active()?;
    if app.auto_tasks.iter().any(|t| t.photo == photo) {
        return Some(Err("Auto is already measuring this photo".into()));
    }
    let input = auto_input(&app.session, photo, regions, control.clone()).ok()?;
    let basis = input.basis.clone();
    let (tx, rx) = channel();
    let done: AutoDone = Box::new(move |r| {
        let _ = tx.send(r);
    });
    if !offload(input, done) {
        return None;
    }
    let row = app.session.activity.start("auto", "Auto Settings", Cancel::No);
    app.auto_tasks.push(AutoTask { photo, basis, control, rx, _row: row });
    Some(Ok(json!({"background": true})))
}

/// Per frame: apply the values that came back, as one undo step, and say so when they could not be.
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    if app.auto_tasks.is_empty() {
        return;
    }
    for task in std::mem::take(&mut app.auto_tasks) {
        let outcome = match task.rx.try_recv() {
            Err(TryRecvError::Empty) => {
                app.auto_tasks.push(task);
                continue;
            }
            Ok(Ok(values)) => {
                let mut params = json!({"id": task.photo.0, "values": values, "basis": task.basis});
                if let Some(control) = &task.control {
                    params["control"] = json!(control);
                }
                // the row ends with the task, before the apply's own result shows
                let AutoTask { _row: row, .. } = task;
                drop(row);
                app.session.execute("develop.applyAutoValues", &params).map(|_| ()).map_err(|e| crate::CommandError::from(e).message)
            }
            Ok(Err(e)) => Err(e),
            Err(TryRecvError::Disconnected) => Err(crate::i18n::tr("Auto Settings stopped unexpectedly").to_string()),
        };
        if let Err(message) = outcome {
            log::warn!("develop.auto: {message}");
            app.ui.status = message.clone();
            app.toast(ctx, message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    use lightcraft_engine::auto_measure::measure_auto;
    use lightcraft_engine::media::{MediaCache, SourceLevel};

    type Taken = Rc<RefCell<Option<(AutoInput, AutoDone)>>>;

    fn app_with_host() -> (LightcraftApp, Taken) {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        let taken: Taken = Rc::default();
        let hand_over = taken.clone();
        app.auto_offload = Some(Rc::new(move |input, done| {
            *hand_over.borrow_mut() = Some((input, done));
            true
        }));
        (app, taken)
    }

    /// What a worker does with the input: its own decode, the measure, no session.
    fn measure(input: &AutoInput) -> RegionAuto {
        let mut media = MediaCache::default();
        let src = media.origin_ref(&input.origin, SourceLevel::Thumb.max_edge()).load_source().unwrap_or_else(|e| panic!("{e}"));
        let info = lightcraft_engine::auto_measure::with_lens_pick(src.info_or(input.info.clone()), input.lens_pick);
        measure_auto(&src.image, &info, &input.settings, &input.faces, input.regions)
    }

    fn develop(app: &LightcraftApp) -> lightcraft_engine::develop::DevelopSettings {
        let id = app.session.active().unwrap_or(PhotoId(0));
        app.session.develop_of(id).map(|d| (*d).clone()).unwrap_or_default()
    }

    // Issue #192: the command hands the measure over and answers at once; a row shows while it is
    // away; what comes back is applied as one undo step.
    #[test]
    fn auto_measures_in_the_host_and_applies_here_as_one_undo_step() {
        let (mut app, taken) = app_with_host();
        let before = develop(&app);
        let out = app.run("develop.auto", json!({})).unwrap();
        assert_eq!(out, json!({"background": true}));
        assert_eq!(develop(&app), before, "nothing is applied until the values are back");
        let rows = app.session.activity.list();
        assert_eq!((rows.len(), rows.first().map(|r| r.kind)), (1, Some("auto")), "{rows:?}");
        assert!(app.run("develop.auto", json!({})).is_err(), "one measure per photo at a time");
        let (input, done) = taken.borrow_mut().take().expect("the host was handed the measure");
        done(Ok(measure(&input)));
        let ctx = egui::Context::default();
        poll(&mut app, &ctx);
        assert_ne!(develop(&app), before);
        assert!(app.session.activity.list().is_empty(), "the row goes with the measure");
        app.session.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(develop(&app), before, "one undo step");
    }

    // the photo was edited while the measure was away: refused, nothing written
    #[test]
    fn values_for_a_photo_that_changed_are_not_applied() {
        let (mut app, taken) = app_with_host();
        app.run("develop.auto", json!({"regions": false})).unwrap();
        let (input, done) = taken.borrow_mut().take().expect("handed over");
        app.session.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5})).unwrap();
        let edited = develop(&app);
        done(Ok(measure(&input)));
        poll(&mut app, &egui::Context::default());
        assert_eq!(develop(&app), edited);
        assert!(app.ui.status.contains("changed"), "{}", app.ui.status);
        assert!(app.auto_tasks.is_empty());
    }

    // a host with no worker declines: the command runs in the session as without a host
    #[test]
    fn a_declining_host_leaves_the_command_to_the_session() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        app.auto_offload = Some(Rc::new(|_, _| false));
        let before = develop(&app);
        let out = app.run("develop.auto", json!({})).unwrap();
        assert!(out.get("exposure").is_some(), "{out}");
        assert_ne!(develop(&app), before);
        assert!(app.auto_tasks.is_empty());
        // a dry run answers with the values, so it never leaves
        let (mut app, taken) = app_with_host();
        let out = app.run("develop.auto", json!({"dryRun": true})).unwrap();
        assert!(out.get("exposure").is_some() && taken.borrow().is_none(), "{out}");
    }

    // a host that drops the measure without an answer ends the row with a message
    #[test]
    fn a_host_that_drops_the_measure_ends_the_row() {
        let (mut app, taken) = app_with_host();
        app.run("develop.autoControl", json!({"control": "light.exposure"})).unwrap();
        drop(taken.borrow_mut().take());
        poll(&mut app, &egui::Context::default());
        assert!(app.auto_tasks.is_empty() && app.session.activity.list().is_empty());
        assert!(!app.ui.status.is_empty(), "the row ends with a message");
    }
}
