//! Real CLI batch measurement: strict manifest rows, digest-bound inputs, per-record failures,
//! create-new output and byte-identical replays.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use lightcraft_photo_ai::digest;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_lightcraft-cli");
const WIDTH: u64 = 24;
const HEIGHT: u64 = 16;
const VALUES: usize = 202;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    path: PathBuf,
    sha: String,
}

impl Fixture {
    fn object(&self, format: &str) -> Value {
        json!({"path": self.path.to_string_lossy(), "sha256": self.sha, "format": format})
    }

    fn region(&self, kind: &str) -> Value {
        json!({"kind": kind, "path": self.path.to_string_lossy(), "sha256": self.sha, "format": "f32-cache"})
    }
}

fn scratch() -> PathBuf {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("ember-measure-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Little-endian u64 width and height, then row-major interleaved f32 samples.
fn cache(width: u64, height: u64, channels: u64, sample: impl Fn(u64, u64, u64) -> f32) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    for y in 0..height {
        for x in 0..width {
            for channel in 0..channels {
                bytes.extend_from_slice(&sample(x, y, channel).to_le_bytes());
            }
        }
    }
    bytes
}

fn image_bytes() -> Vec<u8> {
    cache(WIDTH, HEIGHT, 3, |x, y, channel| match channel {
        0 => x as f32 / WIDTH as f32,
        1 => y as f32 / HEIGHT as f32,
        _ => 0.2 + 0.1 * ((x + y) % 5) as f32,
    })
}

/// A region covering the top half of the frame.
fn alpha_bytes(width: u64, height: u64) -> Vec<u8> {
    cache(width, height, 1, |_, y, _| if y < height / 2 { 1.0 } else { 0.0 })
}

fn png_bytes() -> Vec<u8> {
    let image = lightcraft_raster::Rgba8::from_fn(WIDTH as usize, HEIGHT as usize, |x, y| [(x * 9) as u8, (y * 14) as u8, 100, 255]);
    lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap()
}

fn put(dir: &Path, name: &str, bytes: &[u8]) -> Fixture {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    Fixture { path, sha: digest(bytes) }
}

fn record(id: &str, image: Value, regions: Vec<Value>) -> Value {
    json!({"schema": "ember.measure-input.v1", "id": id, "image": image, "regions": regions})
}

fn write_manifest(dir: &Path, lines: &[String]) -> PathBuf {
    let path = dir.join("manifest.jsonl");
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    path
}

fn run(manifest: &Path, out: &Path) -> Output {
    Command::new(BIN).args(["ai", "measure", "--manifest"]).arg(manifest).arg("--out").arg(out).output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn summary(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

fn read_rows(out: &Path) -> (Value, Vec<Value>) {
    let text = std::fs::read_to_string(out).unwrap();
    let mut rows = text.lines().map(|line| serde_json::from_str::<Value>(line).unwrap());
    let header = rows.next().unwrap();
    (header, rows.collect())
}

fn index_of(header: &Value, name: &str) -> usize {
    header["names"].as_array().unwrap().iter().position(|n| n.as_str() == Some(name)).unwrap()
}

fn values(row: &Value) -> Vec<f64> {
    row["values"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect()
}

fn assert_measured(row: &Value, id: &str) {
    assert_eq!(row["id"], id, "{row}");
    assert!(row.get("error").is_none(), "{row}");
    let numbers = values(row);
    assert_eq!(numbers.len(), VALUES);
    assert!(numbers.iter().all(|v| v.is_finite()));
}

fn assert_failed(row: &Value, id: &str, needle: &str) {
    assert_eq!(row["id"], id, "{row}");
    assert!(row.get("values").is_none(), "{row}");
    let error = row["error"].as_str().unwrap();
    assert!(error.contains(needle), "{error:?} lacks {needle:?}");
}

#[test]
fn header_names_and_rows_hold_202_finite_values_and_regions_register() {
    let dir = scratch();
    let image = put(&dir, "private-image.f32", &image_bytes());
    let sky = put(&dir, "private-sky.f32", &alpha_bytes(WIDTH, HEIGHT));
    let plain = json!({"schema": "ember.measure-input.v1", "id": "plain", "image": image.object("f32-cache")});
    let with_sky = record("with-sky", image.object("f32-cache"), vec![sky.region("sky")]);
    let manifest = write_manifest(&dir, &[plain.to_string(), with_sky.to_string()]);
    let out = dir.join("out.jsonl");

    let result = run(&manifest, &out);
    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(summary(&result), json!({"measured": 2, "failed": 0}));

    let (header, rows) = read_rows(&out);
    assert_eq!(header["schema"], "ember.measure-output.v1");
    assert_eq!(header["measure_schema"], "ember.dense-measure.v1");
    assert_eq!(header["names"].as_array().unwrap().len(), VALUES);
    assert_eq!(rows.len(), 2);
    assert_measured(&rows[0], "plain");
    assert_measured(&rows[1], "with-sky");
    assert_eq!(rows[0]["image_sha256"], image.sha);
    assert_eq!(rows[1]["image_sha256"], image.sha);

    let present = index_of(&header, "region.sky.present");
    let share = index_of(&header, "region.sky.share");
    assert_eq!(values(&rows[0])[present], 0.0);
    assert_eq!(values(&rows[1])[present], 1.0);
    assert!(values(&rows[1])[share] > 0.4);
    assert_eq!(values(&rows[0])[share], 0.0);

    // Ids and digests only: no manifest path or file name reaches the output.
    let text = std::fs::read_to_string(&out).unwrap();
    for leaked in ["private-", "ember-measure-", "manifest.jsonl"] {
        assert!(!text.contains(leaked), "output leaks {leaked}");
        assert!(!String::from_utf8_lossy(&result.stdout).contains(leaked));
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn record_failures_become_error_rows_and_the_batch_continues() {
    let dir = scratch();
    let good = image_bytes();
    let image = put(&dir, "good.f32", &good);
    let odd = put(&dir, "odd.f32", &good[..good.len() - 1]);
    let short = put(&dir, "short.f32", &good[..good.len() - 12]);
    let mut longer = good.clone();
    longer.push(0);
    let extended = put(&dir, "extended.f32", &longer);
    let sky = put(&dir, "sky.f32", &alpha_bytes(WIDTH, HEIGHT));
    let rgb_region = put(&dir, "rgb-region.f32", &good);
    let small_region = put(&dir, "small-region.f32", &alpha_bytes(8, 8));
    let mut raf = b"FUJIFILMCCD-RAW 0201".to_vec();
    raf.extend_from_slice(&[0u8; 64]);
    let raw = put(&dir, "fake.raf", &raf);
    let absent = dir.join("absent.f32");

    let mut wrong_sha = image.object("f32-cache");
    wrong_sha["sha256"] = json!("0".repeat(64));
    let mut bad_format = image.object("f32-cache");
    bad_format["format"] = json!("bmp");
    let mut unknown = record("unknown-field", image.object("f32-cache"), vec![]);
    unknown["extra"] = json!(1);
    let missing = json!({"path": absent.to_string_lossy(), "sha256": "1".repeat(64), "format": "f32-cache"});

    let lines = [
        record("ok-first", image.object("f32-cache"), vec![]).to_string(),
        record("bad-sha", wrong_sha, vec![]).to_string(),
        "{not json".to_string(),
        record("odd-length", odd.object("f32-cache"), vec![]).to_string(),
        record("short-pixel", short.object("f32-cache"), vec![]).to_string(),
        record("extended", extended.object("f32-cache"), vec![]).to_string(),
        record("region-channels", image.object("f32-cache"), vec![rgb_region.region("sky")]).to_string(),
        record("region-size", image.object("f32-cache"), vec![small_region.region("skin")]).to_string(),
        record("region-duplicate", image.object("f32-cache"), vec![sky.region("sky"), sky.region("sky")]).to_string(),
        unknown.to_string(),
        record("raw-file", raw.object("file"), vec![]).to_string(),
        record("missing-file", missing, vec![]).to_string(),
        record("bad-format", bad_format, vec![]).to_string(),
        record("ok-last", image.object("f32-cache"), vec![sky.region("sky")]).to_string(),
    ];
    let manifest = write_manifest(&dir, &lines);
    let out = dir.join("out.jsonl");

    let result = run(&manifest, &out);
    assert!(result.status.success(), "record failures must not fail the run: {}", stderr(&result));
    assert_eq!(summary(&result), json!({"measured": 2, "failed": lines.len() - 2}));

    let (_, rows) = read_rows(&out);
    assert_eq!(rows.len(), lines.len());
    assert_measured(&rows[0], "ok-first");
    assert_failed(&rows[1], "bad-sha", "SHA256");
    assert_failed(&rows[2], "line:3", "not valid JSON");
    assert_failed(&rows[3], "odd-length", "cache object");
    assert_failed(&rows[4], "short-pixel", "cache object");
    assert_failed(&rows[5], "extended", "cache object");
    assert_failed(&rows[6], "region-channels", "channel");
    assert_failed(&rows[7], "region-size", "size");
    assert_failed(&rows[8], "region-duplicate", "duplicate");
    assert_failed(&rows[9], "unknown-field", "unknown field");
    assert_failed(&rows[10], "raw-file", "raw");
    assert_failed(&rows[11], "missing-file", "unreadable");
    assert_failed(&rows[12], "bad-format", "format");
    assert_measured(&rows[13], "ok-last");

    // Error text never carries a path either.
    let text = std::fs::read_to_string(&out).unwrap();
    for leaked in ["absent.f32", "ember-measure-", "fake.raf"] {
        assert!(!text.contains(leaked), "output leaks {leaked}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn oversized_lines_error_while_blank_lines_and_crlf_are_tolerated() {
    let dir = scratch();
    let image = put(&dir, "image.f32", &image_bytes());
    let good = record("after-long-line", image.object("f32-cache"), vec![]).to_string();
    let mut manifest_bytes = "a".repeat(64 * 1024 + 1).into_bytes();
    manifest_bytes.extend_from_slice(b"\n\n   \n");
    manifest_bytes.extend_from_slice(good.as_bytes());
    manifest_bytes.extend_from_slice(b"\r\n");
    let manifest = dir.join("manifest.jsonl");
    std::fs::write(&manifest, manifest_bytes).unwrap();
    let out = dir.join("out.jsonl");

    let result = run(&manifest, &out);
    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(summary(&result), json!({"measured": 1, "failed": 1}));
    let (_, rows) = read_rows(&out);
    assert_eq!(rows.len(), 2);
    assert_failed(&rows[0], "line:1", "exceeds");
    assert_measured(&rows[1], "after-long-line");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn existing_output_is_refused_and_left_untouched() {
    let dir = scratch();
    let image = put(&dir, "image.f32", &image_bytes());
    let manifest = write_manifest(&dir, &[record("one", image.object("f32-cache"), vec![]).to_string()]);
    let out = dir.join("out.jsonl");
    std::fs::write(&out, b"keep me\n").unwrap();

    let result = run(&manifest, &out);
    assert!(!result.status.success());
    assert!(stderr(&result).contains("new measure output"), "{}", stderr(&result));
    assert!(result.stdout.is_empty());
    assert_eq!(std::fs::read(&out).unwrap(), b"keep me\n");

    // The manifest itself is an existing file too.
    let manifest_before = std::fs::read(&manifest).unwrap();
    assert!(!run(&manifest, &manifest).status.success());
    assert_eq!(std::fs::read(&manifest).unwrap(), manifest_before);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn command_level_failures_exit_non_zero_without_creating_output() {
    let dir = scratch();
    let out = dir.join("out.jsonl");

    let missing = run(&dir.join("absent.jsonl"), &out);
    assert!(!missing.status.success());
    assert!(!out.exists());

    let too_many = dir.join("too-many.jsonl");
    std::fs::write(&too_many, vec![b'\n'; 1_000_001]).unwrap();
    let capped = run(&too_many, &out);
    assert!(!capped.status.success());
    assert!(stderr(&capped).contains("exceeds"), "{}", stderr(&capped));
    assert!(!out.exists());

    let manifest = write_manifest(&dir, &[]);
    let bad_arguments: [&[&str]; 4] = [
        &["ai", "measure"],
        &["ai", "measure", "--manifest"],
        &["ai", "measure", "--bogus", "x"],
        &["ai", "measure", "--manifest", "a", "--manifest", "b", "--out", "c"],
    ];
    for args in bad_arguments {
        let result = Command::new(BIN).args(args).output().unwrap();
        assert!(!result.status.success(), "{args:?}");
    }

    // An empty manifest is a completed run: a header and no rows.
    let empty = run(&manifest, &out);
    assert!(empty.status.success(), "{}", stderr(&empty));
    assert_eq!(summary(&empty), json!({"measured": 0, "failed": 0}));
    assert_eq!(read_rows(&out).1.len(), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rendered_file_records_decode_to_the_working_space() {
    let dir = scratch();
    let png = put(&dir, "render.png", &png_bytes());
    let manifest = write_manifest(&dir, &[record("render", png.object("file"), vec![]).to_string()]);
    let out = dir.join("out.jsonl");

    let result = run(&manifest, &out);
    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(summary(&result), json!({"measured": 1, "failed": 0}));
    let (header, rows) = read_rows(&out);
    assert_measured(&rows[0], "render");
    assert_eq!(rows[0]["image_sha256"], png.sha);
    // A rendered 8-bit file has real tone: the median is neither black nor white.
    let median = values(&rows[0])[index_of(&header, "tone.lum.p50")];
    assert!(median > 0.05 && median < 0.95, "{median}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_same_manifest_twice_gives_identical_output_bytes() {
    let dir = scratch();
    let image = put(&dir, "image.f32", &image_bytes());
    let sky = put(&dir, "sky.f32", &alpha_bytes(WIDTH, HEIGHT));
    let png = put(&dir, "render.png", &png_bytes());
    let mut wrong_sha = image.object("f32-cache");
    wrong_sha["sha256"] = json!("f".repeat(64));
    let lines = [
        record("cache", image.object("f32-cache"), vec![]).to_string(),
        record("cache-sky", image.object("f32-cache"), vec![sky.region("sky")]).to_string(),
        record("render", png.object("file"), vec![]).to_string(),
        record("failure", wrong_sha, vec![]).to_string(),
        "[]".to_string(),
    ];
    let manifest = write_manifest(&dir, &lines);
    let (first, second) = (dir.join("first.jsonl"), dir.join("second.jsonl"));

    let a = run(&manifest, &first);
    let b = run(&manifest, &second);
    assert!(a.status.success() && b.status.success(), "{}{}", stderr(&a), stderr(&b));
    assert_eq!(summary(&a), json!({"measured": 3, "failed": 2}));
    assert_eq!(a.stdout, b.stdout);
    let bytes = std::fs::read(&first).unwrap();
    assert_eq!(bytes, std::fs::read(&second).unwrap());
    assert!(bytes.ends_with(b"\n"));
    let (_, rows) = read_rows(&first);
    assert_failed(&rows[4], "line:5", "JSON object");
    std::fs::remove_dir_all(dir).unwrap();
}
