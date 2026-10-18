//! Album CRUD through real CLI JSONL processes, persisted catalog & decoded export.
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_lightcraft-cli");

fn step(command: &str, params: Value) -> Value {
    json!({"command": command, "params": params})
}

fn run(library: &Path, script: &Path, steps: &[Value]) -> Vec<Value> {
    std::fs::write(script, steps.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
    let output = Command::new(BIN).args(["run", "--library"]).arg(library).arg("--script").arg(script).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{}\n{stdout}", String::from_utf8_lossy(&output.stderr));
    let lines: Vec<Value> = stdout.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(lines.len(), steps.len());
    assert!(lines.iter().all(|line| line["ok"] == true), "{lines:?}");
    lines.into_iter().map(|line| line["result"].clone()).collect()
}

#[test]
fn albums_crud_persists_without_changing_photos_or_sources() {
    let root = std::env::temp_dir().join(format!("lightcraft-albums-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("sources");
    std::fs::create_dir_all(&source).unwrap();
    let mut originals = Vec::new();
    for index in 0..24u8 {
        let path = source.join(format!("photo-{index:02}.png"));
        let image = lightcraft_raster::Rgba8::from_fn(96, 64, |x, y| [(x * 2) as u8, (y * 3) as u8, index * 10, 255]);
        let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        originals.push((path, bytes));
    }
    let library = root.join("library");
    let script = root.join("journey.jsonl");
    let imported = run(&library, &script, &[step("library.import", json!({"paths": [source]})), step("catalog.query", json!({"limit": 24}))]);
    assert_eq!(imported[0]["imported"].as_array().unwrap().len(), 24);
    assert_eq!(imported[1]["total"], 24);
    let id = imported[1]["photos"][0]["id"].as_u64().unwrap();
    let created = run(
        &library,
        &script,
        &[
            step("library.select", json!({"ids": [id], "active": id})),
            step("photo.rate", json!({"rating": 5})),
            step("album.create", json!({"name": "Trips", "folder": true})),
        ],
    );
    let folder = created[2]["id"].as_u64().unwrap();
    let child = run(
        &library,
        &script,
        &[
            step("library.select", json!({"ids": [id], "active": id})),
            step("album.create", json!({"name": "Best", "parent": folder, "addSelected": true})),
            step("albums.list", json!({})),
        ],
    );
    let album = child[1]["id"].as_u64().unwrap();
    assert_eq!(child[2][0]["id"], folder);
    assert_eq!(child[2][0]["folder"], true);
    assert_eq!(child[2][0]["children"][0]["id"], album);
    assert_eq!(child[2][0]["children"][0]["count"], 1);
    let journey = run(
        &library,
        &script,
        &[
            step("library.source", json!({"kind": "album", "id": album})),
            step("catalog.query", json!({})),
            step("album.rename", json!({"id": album, "name": "Best of"})),
            step("library.source", json!({"kind": "all"})),
            step("library.selectAll", json!({})),
            step("album.addPhotos", json!({"id": album})),
            step("albums.list", json!({})),
            step("photo.inspect", json!({"id": id})),
            step("album.delete", json!({"id": folder})),
            step("albums.list", json!({})),
            step("catalog.query", json!({"limit": 24})),
            step("edit.undo", json!({})),
            step("albums.list", json!({})),
            step("library.source", json!({"kind": "album", "id": album})),
            step("catalog.query", json!({"limit": 24})),
            step("library.source", json!({"kind": "all"})),
            step("edit.redo", json!({})),
            step("albums.list", json!({})),
            step("catalog.query", json!({"limit": 24})),
        ],
    );
    assert_eq!(journey[1]["total"], 1);
    assert_eq!(journey[1]["photos"][0]["id"], id);
    assert_eq!(journey[6][0]["children"][0]["name"], "Best of");
    assert_eq!(journey[6][0]["children"][0]["count"], 24);
    assert!(journey[7]["albums"].as_array().unwrap().contains(&json!(album)));
    for index in [9, 17] {
        assert_eq!(journey[index], json!([]), "folder deletion must remove both rows");
    }
    for index in [10, 14, 18] {
        assert_eq!(journey[index]["total"], 24);
        assert_eq!(journey[index]["photos"].as_array().unwrap().len(), 24);
    }
    assert_eq!(journey[12], journey[6], "undo must restore complete folder tree & membership");
    let export = root.join("rendered.png");
    let reopened = run(
        &library,
        &script,
        &[
            step("library.source", json!({"kind": "all"})),
            step("albums.list", json!({})),
            step("catalog.query", json!({"limit": 24})),
            step("photo.inspect", json!({"id": id})),
            step("library.select", json!({"ids": [id], "active": id})),
            step("app.export", json!({"path": export, "longEdge": 96, "format": "png"})),
        ],
    );
    assert_eq!(reopened[1], json!([]));
    assert_eq!(reopened[2]["photos"], journey[18]["photos"], "fresh process must preserve every photo rating & identity");
    assert_eq!(reopened[3]["rating"], 5);
    assert_eq!(reopened[3]["albums"], json!([]));
    assert!(originals.iter().any(|(path, _)| path.file_name().unwrap().to_str().unwrap() == reopened[3]["file_name"].as_str().unwrap()));
    assert_eq!(reopened[5]["path"], export.to_string_lossy().as_ref());
    assert_eq!((reopened[5]["width"].as_u64(), reopened[5]["height"].as_u64()), (Some(96), Some(64)));
    let decoded = lightcraft_codecs::decode(&std::fs::read(export).unwrap(), Default::default()).unwrap().to_srgb8();
    assert_eq!((decoded.width, decoded.height), (96, 64));
    assert!(decoded.data.windows(2).any(|pixels| pixels[0] != pixels[1]), "export must contain decoded image detail");
    for (path, bytes) in originals {
        assert_eq!(std::fs::read(path).unwrap(), bytes, "organization & rendering must preserve source bytes");
    }
    std::fs::remove_dir_all(root).unwrap();
}
