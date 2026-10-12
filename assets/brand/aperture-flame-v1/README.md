# Ember aperture + flame icon pack

Selected direction: four charcoal aperture blades & one orange flame.
`source/master-rgba.png` is 1280px original transparent raster master. It is not an SVG.

- `png/color`, `black`, `white`, `reverse`: transparent PNGs at 16, 24, 32, 48, 64, 128, 256, 512 & 1024px. Aperture opening & separators retain alpha.
- `desktop/ember.ico`: Windows container with 16–256px PNG representations.
- `desktop/ember.icns`: macOS container with 16, 32, 64, 128, 256, 512 & 1024px PNG representations.
- `apple/AppIcon.appiconset`: opaque 1024px default, dark & tinted raster fallbacks, plus Xcode Contents.json. System applies final corner mask.
- `preview.png`: contact sheet; backgrounds appear only in preview tiles.
- `manifest.json`: master/output hashes, dimensions & alpha checks. Small-size antialiasing permits up to 16/255 alpha at sampled aperture center.

Apple raster configuration follows https://developer.apple.com/documentation/xcode/configuring-your-app-icon . Transparent marks may also be imported as layers into Icon Composer: https://developer.apple.com/documentation/xcode/creating-your-app-icon-using-icon-composer . No iOS target or packaged application is changed by this export pack.

Regenerate with `packaging/build-aperture-icon-pack.cjs` using Node & Sharp. Geometry & alpha come from one master; monochrome variants use identical alpha. No Rust build or app launch is needed.

Built-in Imagegen edit prompt: extract only large upper symbol from selected original Ember image; remove background, wordmark & lower small mark; preserve four charcoal blades, one orange flame, silhouette, gaps & central opening; output centered symbol on genuine transparency with smooth edges, no extra symbols, text, glow or shadow. Exact prompt & input hash remain in source/provenance.json.
