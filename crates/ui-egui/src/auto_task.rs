//! Auto off the UI thread, for a host that measures elsewhere (#192).
//!
//! Auto is two phases ([`lightcraft_engine::auto_measure`]): measuring the decoded source, which
//! needs no session, and applying the values, which is one undo step on this thread. A host that
//! has somewhere else to measure (the web's render workers) sets [`LightcraftApp::auto_offload`];
//! the Auto commands then hand the measure over ([`intercept`]), show a row in the activity stack
//! while it is away, and apply what comes back ([`poll`]). The apply is refused when the photo
//! changed in the meantime. Without a host, or when it declines, the command runs in the session as
//! before, measure and apply together.
//!
//! Two ways to apply, by command:
//!
//! - `develop.auto`, `develop.autoTone` and `develop.autoControl` apply with
//!   `develop.applyAutoValues` (one photo).
//! - Every other Auto command is run itself once its measures are back: `develop.autoBatch` (one
//!   measure per photo, a few away at a time), `develop.autoBwMix`, `develop.autoNoise`, White
//!   Balance Auto (`develop.wb` with mode auto, `develop.autoWhiteBalance`), `preset.apply` of a
//!   preset that applies Auto, and `develop.auto*` with `dryRun`. The measures are handed to the
//!   session ([`lightcraft_engine::Session::prepare_auto`]) and the command uses them in place of
//!   measuring, so what it writes, and its one undo step, are what they were. It refuses a measure
//!   of a photo that changed since, before it writes anything.
//!
//! A dry run answers with the values, which come back later: the command answers
//! `{"background": true, "task": n}` and the values are kept for [`take_answer`].

use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use lightcraft_engine::activity::{Cancel, TaskGuard};
use lightcraft_engine::auto_measure::{AutoInput, AutoKind, AutoMeasured, auto_input_for, is_auto_control};
use lightcraft_engine::catalog::PhotoId;
use serde_json::{Value, json};

use crate::LightcraftApp;

/// The host's answer: what was measured, or why it could not be. Called once, on the UI thread.
pub type AutoDone = Box<dyn FnOnce(Result<AutoMeasured, String>)>;

/// Measures `input` somewhere that doesn't block the UI thread and reports through the callback.
/// `false`: the host can't (no worker is alive); the command then runs in the session.
pub type AutoOffload = Rc<dyn Fn(AutoInput, AutoDone) -> bool>;

/// How many measures of one task are away at once (a batch offers the next as one comes back).
pub const IN_FLIGHT: usize = 4;

/// Most finished dry-run answers kept for [`take_answer`].
const MAX_ANSWERS: usize = 16;

static NEXT_TASK: AtomicU64 = AtomicU64::new(1);

/// How a task's values reach the photos.
enum Finish {
    /// `develop.applyAutoValues` for the one photo (`control`: that slider only).
    Apply { control: Option<String> },
    /// Hand the measures to the session and run the command itself.
    Run,
}

/// One measure that is away.
struct Flight {
    photo: PhotoId,
    regions: bool,
    basis: String,
    rx: Receiver<Result<AutoMeasured, String>>,
}

/// One measure that is back.
struct Measured {
    photo: PhotoId,
    regions: bool,
    basis: String,
    values: AutoMeasured,
}

/// Measures that are away for one command: what to do with them, and the row that shows it.
pub struct AutoTask {
    id: u64,
    finish: Finish,
    command: String,
    params: Value,
    /// The command answers with the values (a dry run): they are kept under `id`.
    answers: bool,
    photos: Vec<PhotoId>,
    /// Inputs not offered to the host yet.
    waiting: VecDeque<AutoInput>,
    flying: Vec<Flight>,
    done: Vec<Measured>,
    _row: TaskGuard,
}

impl AutoTask {
    /// The first photo being measured.
    pub fn photo(&self) -> Option<PhotoId> {
        self.photos.first().copied()
    }

    /// Every photo being measured.
    pub fn photos(&self) -> &[PhotoId] {
        &self.photos
    }
}

/// What an Auto command needs measured.
struct Plan {
    kind: AutoKind,
    regions: bool,
    control: Option<String>,
    photos: Vec<PhotoId>,
    finish: Finish,
    answers: bool,
    /// The activity row's label (English; the activity stack translates it).
    label: &'static str,
}

fn bool_param(params: &Value, key: &str, default: bool) -> bool {
    params.get(key).and_then(Value::as_bool).unwrap_or(default)
}

/// What command `id` with `params` needs measured, `None` for a command (or a call) that is not the
/// host's: the session runs it, and says why when it should refuse.
fn plan(app: &LightcraftApp, id: &str, params: &Value) -> Option<Plan> {
    let session = &app.session;
    let one = |kind, label| {
        Some(Plan {
            kind,
            regions: false,
            control: None,
            photos: vec![session.active()?],
            finish: Finish::Run,
            answers: false,
            label,
        })
    };
    match id {
        "develop.auto" | "develop.autoTone" => {
            let dry = bool_param(params, "dryRun", false);
            Some(Plan {
                kind: AutoKind::Tone,
                regions: bool_param(params, "regions", true),
                control: None,
                photos: vec![session.active()?],
                finish: if dry { Finish::Run } else { Finish::Apply { control: None } },
                answers: dry,
                label: "Auto Settings",
            })
        }
        "develop.autoControl" => {
            let control = params.get("control").and_then(Value::as_str).filter(|c| is_auto_control(c))?;
            Some(Plan {
                kind: AutoKind::Tone,
                regions: false,
                control: Some(control.to_string()),
                photos: vec![session.active()?],
                finish: Finish::Apply { control: Some(control.to_string()) },
                answers: false,
                label: "Auto Settings",
            })
        }
        "develop.autoBatch" => {
            let photos: Vec<PhotoId> = session.targets(params).ok()?.into_iter().filter(|p| session.catalog.photo(*p).is_some()).collect();
            if photos.is_empty() {
                return None;
            }
            Some(Plan {
                kind: AutoKind::Tone,
                regions: bool_param(params, "regions", true),
                control: None,
                photos,
                finish: Finish::Run,
                answers: false,
                label: "Auto Settings",
            })
        }
        "develop.autoBwMix" => one(AutoKind::BwMix, "Auto B&W Mix"),
        // the only one of these with a dry run: it answers with the numbers, which come back later
        "develop.autoNoise" => one(AutoKind::Noise, "Auto Noise Reduction").map(|p| Plan { answers: bool_param(params, "dryRun", false), ..p }),
        "develop.autoWhiteBalance" => one(AutoKind::WhiteBalance, "Auto White Balance"),
        "develop.wb" if params.get("mode").and_then(Value::as_str) == Some("auto") => one(AutoKind::WhiteBalance, "Auto White Balance"),
        "preset.apply" => {
            let pid = params.get("id").and_then(Value::as_str)?;
            session.presets.iter().find(|p| p.id == pid).filter(|p| p.auto)?;
            // the photos the command acts on: `ids`, else the selection
            let photos: Vec<PhotoId> =
                session.targets(&json!({"ids": params.get("ids")})).ok()?.into_iter().filter(|p| session.develop_of(*p).is_some()).collect();
            if photos.is_empty() {
                return None;
            }
            Some(Plan { kind: AutoKind::Tone, regions: false, control: None, photos, finish: Finish::Run, answers: false, label: "Apply Preset" })
        }
        _ => None,
    }
}

/// Offer the next waiting input to the host. `false`: nothing waiting, or the host declined it (it
/// is dropped; the command measures what was not offered).
fn offer(offload: &AutoOffload, waiting: &mut VecDeque<AutoInput>, flying: &mut Vec<Flight>) -> bool {
    let Some(input) = waiting.pop_front() else { return false };
    let (photo, regions, basis) = (input.photo, input.regions, input.basis.clone());
    let (tx, rx) = channel();
    let done: AutoDone = Box::new(move |r| {
        let _ = tx.send(r);
    });
    if !offload(input, done) {
        return false;
    }
    flying.push(Flight { photo, regions, basis, rx });
    true
}

/// Hand an Auto command to the host's [`LightcraftApp::auto_offload`]. `None`: not for the host
/// (no host, another command, or something the session should refuse in its own words), so the
/// command runs in the session.
pub fn intercept(app: &mut LightcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let offload = app.auto_offload.clone()?;
    let plan = plan(app, id, params)?;
    if plan.photos.iter().any(|p| app.auto_tasks.iter().any(|t| t.photos.contains(p))) {
        return Some(Err("Auto is already measuring this photo".into()));
    }
    let mut waiting = VecDeque::new();
    for photo in &plan.photos {
        waiting.push_back(auto_input_for(&app.session, *photo, plan.kind, plan.regions, plan.control.clone()).ok()?);
    }
    let mut flying = Vec::new();
    // the first one declined: the host has no worker, so the session does it all
    if !offer(&offload, &mut waiting, &mut flying) {
        return None;
    }
    while flying.len() < IN_FLIGHT && !waiting.is_empty() {
        if !offer(&offload, &mut waiting, &mut flying) {
            waiting.clear();
        }
    }
    let row = app.session.activity.start("auto", plan.label, Cancel::No);
    let task = NEXT_TASK.fetch_add(1, Ordering::Relaxed);
    app.auto_tasks.push(AutoTask {
        id: task,
        finish: plan.finish,
        command: id.to_string(),
        params: params.clone(),
        answers: plan.answers,
        photos: plan.photos,
        waiting,
        flying,
        done: Vec::new(),
        _row: row,
    });
    Some(Ok(if plan.answers { json!({"background": true, "task": task}) } else { json!({"background": true}) }))
}

/// What the user reads when a measure ended without an answer.
fn stopped() -> String {
    crate::i18n::tr("Auto Settings stopped unexpectedly").to_string()
}

/// Where a task stands after taking what came back.
enum Step {
    Waiting,
    Done,
    Failed(String),
}

impl AutoTask {
    /// Take the measures that are back, offer the next waiting ones.
    fn advance(&mut self, offload: Option<&AutoOffload>) -> Step {
        let mut failure: Option<String> = None;
        for f in std::mem::take(&mut self.flying) {
            match f.rx.try_recv() {
                Err(TryRecvError::Empty) => self.flying.push(f),
                Ok(Ok(values)) => self.done.push(Measured { photo: f.photo, regions: f.regions, basis: f.basis, values }),
                Ok(Err(e)) => failure = failure.or(Some(e)),
                Err(TryRecvError::Disconnected) => failure = failure.or_else(|| Some(stopped())),
            }
        }
        if let Some(e) = failure {
            return Step::Failed(e);
        }
        while self.flying.len() < IN_FLIGHT && !self.waiting.is_empty() {
            match offload {
                Some(o) if offer(o, &mut self.waiting, &mut self.flying) => {}
                // the host stopped taking them: the command measures the rest itself
                _ => self.waiting.clear(),
            }
        }
        if self.flying.is_empty() && self.waiting.is_empty() { Step::Done } else { Step::Waiting }
    }
}

/// Run the finished task: apply its values. `Ok`: what the command answered.
fn finish(app: &mut LightcraftApp, task: AutoTask) -> Result<Value, String> {
    let AutoTask { finish, command, params, photos, done, _row: row, .. } = task;
    // the row ends with the task, before the apply's own result shows
    drop(row);
    match finish {
        Finish::Apply { control } => {
            let (Some(photo), Some(m)) = (photos.first().copied(), done.into_iter().next()) else { return Err(stopped()) };
            let AutoMeasured::Tone(values) = m.values else { return Err(stopped()) };
            let mut apply = json!({"id": photo.0, "values": values, "basis": m.basis});
            if let Some(control) = control {
                apply["control"] = json!(control);
            }
            app.session.execute("develop.applyAutoValues", &apply).map_err(|e| crate::CommandError::from(e).message)
        }
        Finish::Run => {
            for m in done {
                app.session.prepare_auto(m.photo, m.regions, m.basis, m.values);
            }
            let result = app.session.execute(&command, &params).map_err(|e| crate::CommandError::from(e).message);
            // whatever the command did not use is not for a later one
            app.session.clear_prepared_auto();
            result
        }
    }
}

/// Per frame: take the measures that came back and, once a command's are all in, apply them (one
/// undo step) and say so when they could not be.
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    if app.auto_tasks.is_empty() {
        return;
    }
    let offload = app.auto_offload.clone();
    for mut task in std::mem::take(&mut app.auto_tasks) {
        let (id, answers) = (task.id, task.answers);
        let outcome = match task.advance(offload.as_ref()) {
            Step::Waiting => {
                app.auto_tasks.push(task);
                continue;
            }
            Step::Done => finish(app, task),
            Step::Failed(e) => Err(e),
        };
        if answers {
            // the caller of a dry run reads this
            app.auto_answers.push((id, outcome));
            let excess = app.auto_answers.len().saturating_sub(MAX_ANSWERS);
            app.auto_answers.drain(..excess);
        } else if let Err(message) = outcome {
            log::warn!("auto: {message}");
            app.ui.status = message.clone();
            app.toast(ctx, message);
        }
    }
}

/// The answer of a dry run that was away (`{"background": true, "task": n}`): `None` while it is
/// still measuring, else the values (or why there are none), once.
pub fn take_answer(app: &mut LightcraftApp, task: u64) -> Option<Result<Value, String>> {
    let i = app.auto_answers.iter().position(|(id, _)| *id == task)?;
    Some(app.auto_answers.remove(i).1)
}

/// Whether a dry run's answer is still to come: the task is away, or its answer is waiting.
pub fn answer_pending(app: &LightcraftApp, task: u64) -> bool {
    app.auto_tasks.iter().any(|t| t.id == task) || app.auto_answers.iter().any(|(id, _)| *id == task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    use lightcraft_engine::auto_measure::{measure_kind, with_lens_pick};
    use lightcraft_engine::develop::DevelopSettings;
    use lightcraft_engine::media::MediaCache;

    type Taken = Rc<RefCell<Vec<(AutoInput, AutoDone)>>>;

    fn app_with_host() -> (LightcraftApp, Taken) {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        let taken: Taken = Rc::default();
        let hand_over = taken.clone();
        app.auto_offload = Some(Rc::new(move |input, done| {
            hand_over.borrow_mut().push((input, done));
            true
        }));
        (app, taken)
    }

    /// What a worker does with the input: its own decode, the measure, no session.
    fn measure(input: &AutoInput) -> AutoMeasured {
        let mut media = MediaCache::default();
        let src = media.origin_ref(&input.origin, input.kind.level().max_edge()).load_source().unwrap_or_else(|e| panic!("{e}"));
        let info = with_lens_pick(src.info_or(input.info.clone()), input.lens_pick);
        let m = measure_kind(input.kind, &src.image, &info, &input.settings, &input.faces, input.regions);
        // through JSON, as it crosses the worker boundary
        serde_json::from_str(&serde_json::to_string(&m).unwrap_or_default()).unwrap_or_else(|e| panic!("{e}"))
    }

    /// The host answers everything it was handed, and the UI polls, until the tasks are done.
    fn answer_all(app: &mut LightcraftApp, taken: &Taken) {
        let ctx = egui::Context::default();
        for _ in 0..20 {
            let handed: Vec<_> = std::mem::take(&mut *taken.borrow_mut());
            for (input, done) in handed {
                done(Ok(measure(&input)));
            }
            poll(app, &ctx);
            if app.auto_tasks.is_empty() {
                return;
            }
        }
        panic!("the tasks did not finish");
    }

    fn develop_of(app: &LightcraftApp, id: PhotoId) -> DevelopSettings {
        app.session.develop_of(id).map(|d| (*d).clone()).unwrap_or_default()
    }

    fn develop(app: &LightcraftApp) -> DevelopSettings {
        develop_of(app, app.session.active().unwrap_or(PhotoId(0)))
    }

    fn select(app: &mut LightcraftApp, n: usize) -> Vec<PhotoId> {
        let ids: Vec<PhotoId> = app.session.visible_cloned().into_iter().take(n).collect();
        app.session.execute("library.select", &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>()})).unwrap();
        ids
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
        assert_eq!(taken.borrow().len(), 1, "the host was handed the measure");
        answer_all(&mut app, &taken);
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
        app.session.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5})).unwrap();
        let edited = develop(&app);
        answer_all(&mut app, &taken);
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
        // every other Auto command too
        for (id, params) in [
            ("develop.autoBatch", json!({})),
            ("develop.autoBwMix", json!({})),
            ("develop.autoNoise", json!({})),
            ("develop.autoWhiteBalance", json!({})),
            ("develop.wb", json!({"mode": "auto"})),
            ("develop.auto", json!({"dryRun": true})),
        ] {
            app.run(id, params).unwrap_or_else(|e| panic!("{id}: {}", e.message));
            assert!(app.auto_tasks.is_empty(), "{id}");
        }
        // with a host, a dry run answers with the values (later) and writes nothing
        let (mut app, taken) = app_with_host();
        let before = develop(&app);
        let out = app.run("develop.auto", json!({"dryRun": true})).unwrap();
        assert_eq!(out.get("background"), Some(&json!(true)), "{out}");
        let task = out["task"].as_u64().unwrap();
        assert!(answer_pending(&app, task) && take_answer(&mut app, task).is_none());
        answer_all(&mut app, &taken);
        let answer = take_answer(&mut app, task).unwrap().unwrap();
        assert!(answer.get("exposure").is_some(), "{answer}");
        assert_eq!(develop(&app), before, "measure only");
        assert!(take_answer(&mut app, task).is_none() && !answer_pending(&app, task), "an answer is read once");
    }

    // a host that drops the measure without an answer ends the row with a message
    #[test]
    fn a_host_that_drops_the_measure_ends_the_row() {
        let (mut app, taken) = app_with_host();
        app.run("develop.autoControl", json!({"control": "light.exposure"})).unwrap();
        taken.borrow_mut().clear();
        poll(&mut app, &egui::Context::default());
        assert!(app.auto_tasks.is_empty() && app.session.activity.list().is_empty());
        assert!(!app.ui.status.is_empty(), "the row ends with a message");
    }

    // Auto Settings to Selection: one measure per photo, a few away at a time, one undo step for all
    #[test]
    fn auto_batch_measures_each_photo_in_the_host_and_applies_once() {
        let (mut app, taken) = app_with_host();
        let ids = select(&mut app, IN_FLIGHT + 2);
        let before: Vec<_> = ids.iter().map(|i| develop_of(&app, *i)).collect();
        let mut native = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        select(&mut native, IN_FLIGHT + 2);
        native.run("develop.autoBatch", json!({"regions": false})).unwrap();

        assert_eq!(app.run("develop.autoBatch", json!({"regions": false})).unwrap(), json!({"background": true}));
        assert_eq!(taken.borrow().len(), IN_FLIGHT, "no more than {IN_FLIGHT} are away at once");
        assert!(ids.iter().all(|i| app.run("develop.autoBatch", json!({"ids": [i.0]})).is_err()), "every photo of the batch is being measured");
        for (id, b) in ids.iter().zip(&before) {
            assert_eq!(develop_of(&app, *id), *b, "nothing is applied until every measure is back");
        }
        answer_all(&mut app, &taken);
        for id in &ids {
            assert_eq!(develop_of(&app, *id), develop_of(&native, *id), "photo {}", id.0);
        }
        app.session.execute("edit.undo", &json!({})).unwrap();
        for (id, b) in ids.iter().zip(&before) {
            assert_eq!(develop_of(&app, *id), *b, "one undo step for the batch");
        }
    }

    // a photo of the batch is edited while its measure is away: the batch is refused, none is written
    #[test]
    fn auto_batch_with_a_changed_photo_writes_nothing() {
        let (mut app, taken) = app_with_host();
        let ids = select(&mut app, 3);
        app.run("develop.autoBatch", json!({})).unwrap();
        app.session.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5})).unwrap();
        let after: Vec<_> = ids.iter().map(|i| develop_of(&app, *i)).collect();
        answer_all(&mut app, &taken);
        for (id, d) in ids.iter().zip(&after) {
            assert_eq!(develop_of(&app, *id), *d, "photo {}", id.0);
        }
        assert!(app.ui.status.contains("changed"), "{}", app.ui.status);
        assert!(app.session.activity.list().is_empty());
        assert!(app.auto_tasks.is_empty());
    }

    // B&W Mix, Noise Reduction and White Balance Auto: measured in the host, applied as the command does
    #[test]
    fn single_photo_auto_commands_measure_in_the_host_and_match_the_native_run() {
        for (id, params) in [
            ("develop.autoBwMix", json!({})),
            ("develop.autoNoise", json!({})),
            ("develop.autoWhiteBalance", json!({})),
            ("develop.wb", json!({"mode": "auto"})),
        ] {
            let (mut app, taken) = app_with_host();
            let mut native = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
            let before = develop(&app);
            native.run(id, params.clone()).unwrap_or_else(|e| panic!("{id}: {}", e.message));
            assert_eq!(app.run(id, params).unwrap(), json!({"background": true}), "{id}");
            assert_eq!(develop(&app), before, "{id}: nothing is applied until the values are back");
            assert_eq!(taken.borrow().len(), 1, "{id}");
            answer_all(&mut app, &taken);
            assert_eq!(develop(&app), develop(&native), "{id}");
            app.session.execute("edit.undo", &json!({})).unwrap();
            assert_eq!(develop(&app), before, "{id}: one undo step");
            assert!(app.session.activity.list().is_empty(), "{id}");
        }
        // a white balance that measures nothing never leaves
        let (mut app, taken) = app_with_host();
        app.run("develop.wb", json!({"mode": "daylight"})).unwrap();
        assert!(taken.borrow().is_empty() && app.auto_tasks.is_empty());
    }

    // the Auto step of a preset: measured in the host, then the preset on top, one undo step
    #[test]
    fn a_preset_with_auto_measures_in_the_host_and_applies_once() {
        let setup = || {
            let (mut app, taken) = app_with_host();
            let ids = select(&mut app, 2);
            let made = app.run("preset.create", json!({"name": "Auto Detail", "groups": ["detail"], "auto": true})).unwrap();
            let pid = made["id"].as_str().unwrap_or_default().to_string();
            // a preset without Auto is not for the host
            let plain = app.run("preset.create", json!({"name": "Plain", "groups": ["detail"]})).unwrap();
            let plain = plain["id"].as_str().unwrap_or_default().to_string();
            app.run("preset.apply", json!({"id": plain})).unwrap();
            assert!(taken.borrow().is_empty() && app.auto_tasks.is_empty(), "no Auto step, nothing to measure");
            (app, taken, ids, pid)
        };
        let (mut app, taken, ids, pid) = setup();
        let (mut native, _, _, _) = setup();
        let before: Vec<_> = ids.iter().map(|i| develop_of(&app, *i)).collect();
        native.session.execute("preset.apply", &json!({"id": pid})).unwrap();

        assert_eq!(app.run("preset.apply", json!({"id": pid})).unwrap(), json!({"background": true}));
        assert_eq!(taken.borrow().len(), 2);
        answer_all(&mut app, &taken);
        for id in &ids {
            assert_eq!(develop_of(&app, *id), develop_of(&native, *id), "photo {}", id.0);
        }
        app.session.execute("edit.undo", &json!({})).unwrap();
        for (id, b) in ids.iter().zip(&before) {
            assert_eq!(develop_of(&app, *id), *b, "one undo step for both photos");
        }
    }
}
