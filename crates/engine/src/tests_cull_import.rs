//! `library.import` with `cull: true`: Assisted Culling's read-only pass covers exactly the photos
//! the import added, and a stopped pass never changes the import or the photos.

use std::path::Path;

use serde_json::{Value, json};

use crate::Session;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-cull-import-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A small procedural PNG (distinct per seed).
fn write_png(path: &Path, seed: u8) {
    let (w, h) = (48usize, 32usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 5) as u8, (i / w * 7) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_768_370_328);
    std::fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
}

fn ids_in(v: &Value, key: &str) -> Vec<u64> {
    // `imported` lists ids; `photos` lists proposal rows keyed by `id`
    let mut ids: Vec<u64> = v[key]
        .as_array()
        .map(|rows| rows.iter().filter_map(|row| row["id"].as_u64().or_else(|| row.as_u64())).collect())
        .unwrap_or_default();
    ids.sort_unstable();
    ids
}

#[test]
fn import_with_cull_analyzes_exactly_the_imported_photos() {
    let src = temp_dir("exact");
    write_png(&src.join("a.png"), 1);
    write_png(&src.join("trip/b.png"), 2);
    write_png(&src.join("trip/c.png"), 3);
    std::fs::copy(src.join("a.png"), src.join("trip/a-copy.png")).unwrap(); // duplicate: not imported, not analyzed

    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "cull": true})).unwrap();
    let imported = ids_in(&r, "imported");
    assert_eq!(imported.len(), 3, "{r}");
    assert_eq!(ids_in(&r["culling"], "photos"), imported, "{r}");
    assert!(r["culling"]["stopped"].is_null(), "{r}");
    assert_eq!(r["culling"]["failed"].as_array().map(Vec::len), Some(0), "{r}");
    // read-only: the pass writes no flags, so every imported photo keeps its unflagged state
    for id in &imported {
        let photo = s.catalog.photo(lightcraft_catalog::PhotoId(*id)).unwrap();
        assert_eq!(photo.flag, lightcraft_catalog::Flag::None);
    }
    let _ = std::fs::remove_dir_all(&src);
}

#[test]
fn import_without_cull_runs_no_culling_pass() {
    let src = temp_dir("off");
    write_png(&src.join("a.png"), 4);
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!(ids_in(&r, "imported").len(), 1, "{r}");
    assert!(r.get("culling").is_none(), "{r}");
    let _ = std::fs::remove_dir_all(&src);
}

#[test]
fn cancelled_culling_pass_stops_before_measuring_and_keeps_the_import() {
    let src = temp_dir("cancel");
    write_png(&src.join("a.png"), 5);
    write_png(&src.join("b.png"), 6);
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let ids: Vec<lightcraft_catalog::PhotoId> = ids_in(&r, "imported").into_iter().map(lightcraft_catalog::PhotoId).collect();
    assert_eq!(ids.len(), 2, "{r}");
    let job = s.plan_cull_job(&ids, None, false).unwrap();
    // a stop request at the first progress check measures nothing
    assert_eq!(job.run(&|_, _| false).err().as_deref(), Some("cancelled"));
    // the import itself is untouched by the stopped pass
    assert_eq!(s.catalog.len(), 2);
    let _ = std::fs::remove_dir_all(&src);
}
