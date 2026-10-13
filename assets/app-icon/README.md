# Ember app icon

Approved Orthic Labs aperture + single orange flame. Canonical RGBA artwork: `../brand/aperture-flame-v1/source/master-rgba.png`; provenance & hashes accompany its pack.

- `ember-1024.png`: transparent documentation/store artwork.
- `ember-macos-512.png`, `ember.icns`: macOS transparent-margin variants.
- `ember.ico`: Windows 16–256 px icon family.
- `hicolor/<size>/apps/com.orthiclabs.ember.png`: Linux 16–512 px icons.
- `ember.svg`, `hicolor/scalable/apps/com.orthiclabs.ember.svg`: compatibility wrappers embedding exact PNG artwork; raster, not vector.
- `ember-reverse-1024.png`: light blades + orange flame for dark installer artwork.

Run `packaging/icons.sh` with Python 3 & Pillow to publish approved master, including self-contained WASM favicon. Tauri About, desktop favicon, native windows, installers & README consume these paths. Apple default/dark/tinted assets remain in `../brand/aperture-flame-v1/apple/AppIcon.appiconset` for future iOS implementation. Licence: MIT OR Apache-2.0; see `LICENSE.txt`, root licence files & `assets/ATTRIBUTION.md`.
