//! AI masks without the SAM 3 model (it is never required): every request is a clear error that
//! comes back at once, nothing waits on the network or the model, a damaged model is an error
//! (not a crash or a stuck "busy"), and the download command needs consent and fails cleanly.
//! No test touches the internet: downloads go to a local server.

use std::time::{Duration, Instant};

use serde_json::json;

use crate::Session;
use crate::segment::{NOT_INSTALLED, Segmenter};

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-sam3-engine-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// A folder that looks installed but holds no usable model.
fn damaged_model(name: &str) -> std::path::PathBuf {
    let d = tmp(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("model.safetensors"), b"\x10\x00\x00\x00\x00\x00\x00\x00{not json at all}").unwrap();
    std::fs::write(d.join("vocab.json"), b"{}").unwrap();
    std::fs::write(d.join("merges.txt"), b"#version: 0.2\n").unwrap();
    d
}

/// Poll until the worker is idle (or `limit` passes), collecting messages.
fn settle(s: &mut Session, limit: Duration) -> Vec<String> {
    let t = Instant::now();
    let mut messages = Vec::new();
    loop {
        let p = s.segment_poll();
        messages.extend(p.messages);
        if !s.segmenter.busy() && !s.segmenter.detail_busy() {
            messages.extend(s.segment_poll().messages);
            return messages;
        }
        assert!(t.elapsed() < limit, "the worker never finished");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn status_and_model_commands_without_the_model() {
    let mut s = Session::with_demo();
    let st = s.execute("segment.model.status", &json!({})).unwrap();
    assert_eq!(st["available"], Segmenter::AVAILABLE);
    assert_eq!(st["installed"], false);
    assert_eq!(st["download"]["running"], false);
    assert!(st["license"].as_str().unwrap().contains("Apache"));
    assert_eq!((st["sizeBytes"].as_u64(), st["model"].is_null(), st["describe"].as_bool()), (Some(40_613_024), true, Some(false)));
    // downloading needs the user's consent, and names the size and the licence
    let e = s.execute("segment.model.download", &json!({})).unwrap_err().to_string();
    assert!(e.contains("acknowledged") && e.contains("Apache License") && e.contains("40.6 MB"), "{e}");
    let e = s.execute("segment.model.download", &json!({"acknowledged": "yes"})).unwrap_err().to_string();
    assert!(e.contains("acknowledged"), "{e}");
    assert_eq!(s.execute("segment.model.cancel", &json!({})).unwrap()["cancelled"], false);
    if !Segmenter::AVAILABLE {
        assert!(s.execute("segment.model.download", &json!({"acknowledged": true})).is_err());
        return;
    }
    // no folder, then no download location: clear errors, nothing started
    let e = s.execute("segment.model.download", &json!({"acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("no folder"), "{e}");
    let dir = tmp("nomirrors");
    s.segmenter.dir = Some(dir.clone());
    s.segmenter.mirrors_file = Some(dir.join("none.txt"));
    // the built-in location is Hugging Face: with it, a download is possible (and not started here)
    #[cfg(feature = "sam")]
    assert_eq!(s.segmenter.mirrors(), lightcraft_segment::fetch::DEFAULT_MIRRORS.iter().map(ToString::to_string).collect::<Vec<_>>());
    s.segmenter.default_mirrors = false;
    if s.segmenter.mirrors().is_empty() {
        let e = s.execute("segment.model.download", &json!({"acknowledged": true})).unwrap_err().to_string();
        assert!(e.contains("LIGHTCRAFT_SAM3_MIRRORS"), "{e}");
    }
    assert!(!s.segmenter.download_status().running);
}

#[cfg(feature = "sam")]
#[test]
fn selecting_a_model_folder_verifies_all_files_before_switching() {
    let dir = damaged_model("select-invalid");
    let mut s = Session::with_demo();
    let error = s.execute("segment.model.selectFolder", &json!({"path": dir})).unwrap_err().to_string();
    assert!(error.contains("official SAM 3 checkpoint size") || error.contains("safetensors"), "{error}");
    assert!(s.segmenter.dir.is_none(), "invalid folder must not replace configured folder");
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(feature = "sam")]
#[test]
fn a_failing_download_ends_with_an_error_and_never_blocks() {
    use std::io::{BufRead, BufReader, Write};
    // a local mirror that has nothing
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/sam3", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut c in l.incoming().flatten() {
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut line = String::new();
            while r.read_line(&mut line).unwrap_or(0) > 2 {
                line.clear();
            }
            let _ = c.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 13\r\nConnection: close\r\n\r\n<h1>404</h1>\n");
        }
    });
    let dir = tmp("download404");
    let mirrors = dir.with_extension("mirrors.txt");
    std::fs::write(&mirrors, format!("# test mirror\n{base}\n")).unwrap();
    let mut s = Session::with_demo();
    s.segmenter.dir = Some(dir.clone());
    s.segmenter.mirrors_file = Some(mirrors.clone());
    let t = Instant::now();
    let r = s.execute("segment.model.download", &json!({"acknowledged": true})).unwrap();
    assert_eq!(r["started"], true);
    assert!(t.elapsed() < Duration::from_secs(1), "starting a download returns at once");
    // a second request while it runs starts nothing
    let again = s.execute("segment.model.download", &json!({"acknowledged": true})).unwrap();
    assert!(again["started"] == false || again["downloading"] == false);
    let t = Instant::now();
    while s.segmenter.download_status().running {
        assert!(t.elapsed() < Duration::from_secs(30), "the download never ended");
        // AI mask requests meanwhile: an error at once, never a wait
        let q = Instant::now();
        let e = s.execute("mask.add", &json!({"kind": "prompt", "text": "sky"})).unwrap_err().to_string();
        assert!(e.contains(NOT_INSTALLED), "{e}");
        assert!(q.elapsed() < Duration::from_millis(500));
        std::thread::sleep(Duration::from_millis(20));
    }
    let st = s.segmenter.download_status();
    assert!(!st.finished);
    assert!(!st.cancelled, "a failure is not a cancel");
    let e = st.error.unwrap();
    assert!(e.contains("404"), "{e}");
    assert!(!s.segmenter.installed());
    // nothing was left behind under a real name, and no error page was saved
    for f in [
        "model.safetensors",
        "vocab.json",
        "merges.txt",
        "vocab.json.part",
        "mobile_sam-tiny-vitt.safetensors",
        "mobile_sam-tiny-vitt.safetensors.part",
    ] {
        assert!(!dir.join(f).exists(), "{f}");
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&mirrors);
}

/// The app (background mode) never waits: with the model missing every request fails at once;
/// with a damaged model the requests are queued, fail on the worker, and come back as messages.
#[test]
fn background_requests_never_wait_and_failures_come_back_as_messages() {
    let mut s = Session::with_demo();
    s.segmenter.background = true;
    let id = s.active().unwrap();
    s.segmenter.dir = Some(tmp("missing"));
    for (cmd, p) in [("mask.add", json!({"kind": "object"})), ("mask.add", json!({"kind": "prompt", "text": "sky"})), ("segment.prepare", json!({}))]
    {
        let t = Instant::now();
        let e = s.execute(cmd, &p).unwrap_err().to_string();
        assert!(t.elapsed() < Duration::from_millis(500), "{cmd}");
        if Segmenter::AVAILABLE {
            assert!(e.starts_with(NOT_INSTALLED), "{cmd}: {e}");
        } else {
            assert!(e.contains("not available"), "{cmd}: {e}");
        }
    }
    assert!(s.develop_of(id).unwrap().masks.is_empty());
    if !Segmenter::AVAILABLE {
        return;
    }
    // a model folder whose weights are damaged
    let dir = damaged_model("damaged");
    s.segmenter.dir = Some(dir.clone());
    assert!(s.segmenter.installed());
    let t = Instant::now();
    s.execute("mask.add", &json!({"kind": "object"})).unwrap();
    let r = s.execute("mask.objectPoint", &json!({"x": 0.5, "y": 0.5})).unwrap();
    assert_eq!(r["pending"], true);
    assert!(s.segmenter.pending_clicks().is_some_and(|p| p.hint.len() == 1));
    // a second click while the first is on its way builds on it
    let r = s.execute("mask.objectPoint", &json!({"x": 0.6, "y": 0.5, "exclude": true})).unwrap();
    assert_eq!((r["include"].as_u64(), r["exclude"].as_u64()), (Some(1), Some(1)));
    let r = s.execute("mask.add", &json!({"kind": "prompt", "text": "the sky"})).unwrap();
    assert_eq!(r["pending"], true);
    assert!(t.elapsed() < Duration::from_secs(1), "nothing waited for the model: {:?}", t.elapsed());
    let messages = settle(&mut s, Duration::from_secs(60));
    assert!(!messages.is_empty() && messages.iter().all(|m| !m.is_empty()), "{messages:?}");
    assert!(messages.iter().any(|m| m.contains("model.safetensors") || m.contains("SAM 3")), "{messages:?}");
    assert!(s.segmenter.pending_clicks().is_none());
    // the Object mask is there, without a selection; no Describe mask was made from nothing
    let d = s.develop_of(id).unwrap();
    assert_eq!(d.masks.len(), 1);
    assert!(matches!(&d.masks[0].components[0].shape, lightcraft_develop::MaskShape::Object { seg: None, .. }));
    assert!(!s.segmenter.busy() && !s.segmenter.loaded());
    let _ = std::fs::remove_dir_all(&dir);
}

/// CLI / MCP (waiting mode): a damaged model is an error from the command, quickly.
#[test]
fn waiting_requests_with_a_damaged_model_are_errors() {
    if !Segmenter::AVAILABLE {
        return;
    }
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    let dir = damaged_model("damaged-wait");
    s.segmenter.dir = Some(dir.clone());
    let e = s.execute("mask.add", &json!({"kind": "prompt", "text": "sky"})).unwrap_err().to_string();
    assert!(e.contains("model.safetensors") || e.contains("SAM 3"), "{e}");
    let e = s.execute("mask.add", &json!({"kind": "object", "points": [[0.5, 0.5]]})).unwrap_err().to_string();
    assert!(!e.is_empty());
    assert!(s.develop_of(id).unwrap().masks.is_empty());
    assert!(!s.segmenter.busy());
    // a stored segmentation needs no model at all (replayed or pasted masks)
    let seg = serde_json::to_value(lightcraft_develop::SegMask::from_logits(4, &[5.0; 16])).unwrap();
    s.segmenter.dir = None;
    s.execute("mask.add", &json!({"kind": "prompt", "text": "sky", "seg": seg})).unwrap();
    assert_eq!(s.develop_of(id).unwrap().masks.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Too many clicks on one Object selection are refused, not sent to the model.
#[test]
fn object_clicks_are_capped() {
    if !Segmenter::AVAILABLE {
        return;
    }
    let mut s = Session::with_demo();
    s.segmenter.background = true;
    let dir = damaged_model("clicks");
    s.segmenter.dir = Some(dir.clone());
    let id = s.active().unwrap();
    let mut d = (*s.develop_of(id).unwrap()).clone();
    let many = vec![lightcraft_geom::Point::new(0.5, 0.5); crate::segment::MAX_CLICKS];
    d.masks.push(lightcraft_develop::Mask {
        id: 1,
        components: vec![lightcraft_develop::MaskComponent {
            name: None,
            op: lightcraft_develop::MaskOp::Add,
            invert: false,
            shape: lightcraft_develop::MaskShape::Object { hint: many, exclude: vec![], seg: None, detail: vec![], edge: 0.0 },
        }],
        ..Default::default()
    });
    s.set_develop(id, d, "test").unwrap();
    s.active_mask = Some(1);
    let e = s.execute("mask.objectPoint", &json!({"x": 0.5, "y": 0.5})).unwrap_err().to_string();
    assert!(e.contains("at most"), "{e}");
    assert!(!s.segmenter.busy());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "sam")]
#[test]
fn a_running_model_download_shows_in_activity_and_cancels() {
    use std::io::{BufRead, BufReader, Write};
    // a local mirror that sends its files slowly, so the download is still running when it is cancelled
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/sam3", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut c in l.incoming().flatten() {
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut line = String::new();
            while r.read_line(&mut line).unwrap_or(0) > 2 {
                line.clear();
            }
            // (the size the pinned MobileSAM file has: a server announcing another one is refused at once)
            let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 40613024\r\nConnection: close\r\n\r\n");
            for _ in 0..250 {
                if c.write_all(&[b' '; 4000]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    });
    let dir = tmp("download-activity");
    let mirrors = dir.with_extension("activity-mirrors.txt");
    std::fs::write(&mirrors, format!("# test mirror\n{base}\n")).unwrap();
    let mut s = Session::with_demo();
    s.segmenter.dir = Some(dir.clone());
    s.segmenter.mirrors_file = Some(mirrors.clone());
    assert_eq!(s.execute("segment.model.download", &json!({"acknowledged": true})).unwrap()["started"], true);
    let rows = s.activity.list();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].detail.as_str()), ("download", "Downloading model", "MobileSAM"));
    assert_eq!(rows[0].unit, crate::activity::Unit::Bytes);
    assert!(rows[0].cancellable);
    s.execute("activity.cancel", &json!({"id": rows[0].id})).unwrap();
    let t = Instant::now();
    while s.segmenter.download_status().running || !s.activity.list().is_empty() {
        assert!(t.elapsed() < Duration::from_secs(10), "the download never stopped");
        std::thread::sleep(Duration::from_millis(10));
    }
    let st = s.segmenter.download_status();
    assert!(st.cancelled, "the app tells a cancel by the flag (#1046): {st:?}");
    let e = st.error.unwrap_or_default();
    assert!(e.contains("cancelled"), "{e}");
    assert!(!s.segmenter.installed());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&mirrors);
}

/// Frontends decide on the error's kind, not its words (#1046): a missing model is
/// `ModelMissing(Sam3)` whether the app queues requests (background) or a command waits (CLI, MCP);
/// a build without AI masks is not a missing model (there is nothing to download).
#[test]
fn a_missing_model_is_model_missing_by_kind() {
    use crate::{AiModel, ErrorKind};
    for background in [true, false] {
        let mut s = Session::with_demo();
        s.segmenter.background = background;
        s.segmenter.dir = Some(tmp(&format!("kind-{background}")));
        for (cmd, p) in
            [("mask.add", json!({"kind": "object"})), ("mask.add", json!({"kind": "prompt", "text": "sky"})), ("segment.prepare", json!({}))]
        {
            let e = s.execute(cmd, &p).unwrap_err();
            if Segmenter::AVAILABLE {
                assert_eq!(e.kind(), ErrorKind::ModelMissing(AiModel::Sam3), "{cmd} (background {background}): {e}");
                assert!(e.to_string().starts_with(NOT_INSTALLED), "the message is unchanged: {e}");
            } else {
                assert_eq!(e.kind(), ErrorKind::Other, "{cmd}: {e}");
            }
        }
    }
}

/// MobileSAM, the default model, alone in the folder: Object works (a damaged file is an error naming the
/// file), Describe says it needs a grounding model at once (issue #221), and the status says which model it is.
#[cfg(feature = "sam")]
#[test]
fn a_mobilesam_folder_is_installed_and_describe_waits_for_grounding() {
    let d = tmp("mobilesam");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("mobile_sam-tiny-vitt.safetensors"), b"\x10\x00\x00\x00\x00\x00\x00\x00{not json at all}").unwrap();
    for background in [true, false] {
        let mut s = Session::with_demo();
        s.segmenter.background = background;
        s.segmenter.dir = Some(d.clone());
        assert!(s.segmenter.installed());
        assert_eq!(s.segmenter.installed_model(), Some("MobileSAM"));
        let id = s.active().unwrap();
        let t = Instant::now();
        let e = s.execute("mask.add", &json!({"kind": "prompt", "text": "sky"})).unwrap_err().to_string();
        assert!(e.contains("Describe") && e.contains("#221") && e.contains("SAM 3"), "background {background}: {e}");
        assert!(t.elapsed() < Duration::from_millis(500), "an error at once, never a wait");
        assert!(s.develop_of(id).unwrap().masks.is_empty());
        let st = s.execute("segment.model.status", &json!({})).unwrap();
        assert_eq!((st["installed"].as_bool(), st["model"].as_str(), st["describe"].as_bool()), (Some(true), Some("MobileSAM"), Some(false)));
    }
    // a damaged MobileSAM: the object request fails on the worker with the file named, never a crash
    let mut s = Session::with_demo();
    s.segmenter.dir = Some(d.clone());
    let e = s.execute("mask.add", &json!({"kind": "object", "points": [[0.5, 0.5]]})).unwrap_err().to_string();
    assert!(e.contains("mobile_sam-tiny-vitt.safetensors"), "{e}");
    assert!(!s.segmenter.busy());
    // selecting the folder verifies the pinned size first
    let e = s.execute("segment.model.selectFolder", &json!({"path": d})).unwrap_err().to_string();
    assert!(e.contains("official MobileSAM checkpoint size"), "{e}");
    let _ = std::fs::remove_dir_all(&d);
}
