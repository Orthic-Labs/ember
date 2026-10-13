# Ember branding & compatibility

Ember is Orthic Labs’ independently maintained LightCraft fork. Product labels, About/title text, translated product names, original flame assets, package metadata & desktop registrations use Ember. Bundle/application ID: `com.orthiclabs.ember`. macOS destination: `/Applications/Ember.app`. No GitHub repository rename is part of this change.

## Existing data

- **Library:** `EMBER_LIBRARY` takes priority, then existing `LIGHTCRAFT_LIBRARY`. Without either override, reuse `~/Pictures/LightCraft Library` when present; otherwise use `~/Pictures/Ember Library`. An existing Ember directory wins. No directory is moved or overwritten.
- **Engine config/models/profiles:** prefer Ember config directories, otherwise reuse existing `LightCraft` (macOS/Windows) or `lightcraft` (Linux) directories. This preserves downloaded models, camera profiles & engine settings.
- **React/Tauri preferences:** if Ember `ui.json` is absent, copy validated JSON from the first existing legacy primary, preview or egui preferences file. Order: `ai.storyteller.lightcraft`, `ai.storyteller.lightcraft.preview`, `LightCraft`. Keep unknown fields & source file; an existing destination is left untouched. Malformed/unreadable legacy preferences return a warning & remain intact.
- **Catalogs & presets:** no format/schema migration. Catalog signatures, `.lcpreset`, `lightcraft.preset`, XMP namespace `http://ns.lightcraft.app/lc/1.0/` & preview/control protocols retain existing identifiers. Export software metadata now identifies Ember.

Internal `lightcraft-*` crates/binaries, frontend `lc-*` classes, persisted browser storage keys, environment variables & generated workflow/artifact IDs remain compatibility interfaces. Their names do not imply upstream product endorsement. Installed-baseline qualification receipts & contributor records retain historical names; changing them would falsify provenance.

New bundle identity registers Ember separately from an installed LightCraft copy. Source changes do not delete existing installs or create more preview copies. Deployment waits for qualified native CI artifacts.

## Original artwork

Approved artwork is `assets/brand/aperture-flame-v1/source/master-rgba.png`: aperture blades + one orange flame. `packaging/icons.sh` publishes this raster master into macOS/Windows/Linux assets & browser favicons. `assets/app-icon/ember.svg` embeds PNG artwork; it is not vector artwork. DMG background uses its reverse variant on charcoal. Attribution & licences are recorded in `assets/ATTRIBUTION.md` & `assets/app-icon/LICENSE.txt`.

ArtCraft marks are removed. `docs/brand/LICENSE-brand.txt` remains only as upstream legal provenance. Plain-text LightCraft/ArtCraft attribution, copyrights, licence notices & contributor/model credits are preserved.
