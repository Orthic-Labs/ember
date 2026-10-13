//! Real offline CLI plane round-trip & create-new/source-identity guards.
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use lightcraft_photo_ai::digest;
use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value, tags};

#[test]
fn grayscale_plane_cli_preserves_input_codes_and_rejects_overwrite_or_wrong_hash() {
    let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("ember-plane-{}-{unique}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let input = root.join("input.tiff");
    let output = root.join("plane.pgm");
    let mut image = IfdBuilder::new()
        .with(tags::IMAGE_WIDTH, Value::Long(vec![2]))
        .with(tags::IMAGE_LENGTH, Value::Long(vec![2]))
        .with(tags::BITS_PER_SAMPLE, Value::Short(vec![8]))
        .with(tags::SAMPLES_PER_PIXEL, Value::Short(vec![1]))
        .with(tags::PHOTOMETRIC, Value::Short(vec![1]));
    image.set_image(ImageData::Strips { rows_per_strip: 2, strips: vec![vec![0, 1, 42, 255]] });
    let bytes = TiffWriter::new(ByteOrder::Little, false).write(&[image]).unwrap();
    std::fs::write(&input, &bytes).unwrap();
    let sha = digest(&bytes);
    let run = |hash: &str, path: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_lightcraft-cli"))
            .args(["ai", "decision", "plane", "--input"])
            .arg(&input)
            .args(["--sha256", hash, "--out"])
            .arg(path)
            .output()
            .unwrap()
    };
    let wrong = root.join("wrong.pgm");
    assert!(!run(&"0".repeat(64), &wrong).status.success());
    assert!(!wrong.exists());
    let result = run(&sha, &output);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let pgm = std::fs::read(&output).unwrap();
    assert_eq!(pgm, b"P5\n2 2\n255\n\x00\x01\x2a\xff");
    let receipt: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(receipt["input_sha256"], sha);
    assert_eq!(receipt["pgm_sha256"], digest(&pgm));
    assert_eq!(receipt["decoder_threads"], 1);
    assert_eq!(receipt["image_registration_verified"], false);
    assert!(!run(&sha, &output).status.success());
    assert!(!run(&sha, &input).status.success());
    assert_eq!(std::fs::read(&input).unwrap(), bytes);
    assert_eq!(std::fs::read(&output).unwrap(), pgm);
    std::fs::remove_dir_all(root).unwrap();
}
