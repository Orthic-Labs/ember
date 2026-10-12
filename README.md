<p align="center"><img src="assets/app-icon/ember-1024.png" width="112" alt="Ember flame icon"></p>

# Ember

Photo library & non-destructive RAW editor, maintained by **Orthic Labs**. Ember is based on [LightCraft](https://github.com/storytold/lightcraft) by ArtCraft Team & contributors, with our React/Tauri desktop workspace & RightKit integrations.

## Workspace

- **Library:** grid, bottom filmstrip, folders/albums, search, ratings, flags & metadata.
- **Develop:** left navigation & right editing panels, history, presets, crop, tone/color controls & masks. Rust processing stays authoritative; UI commands use shared engine contracts.
- **Import/export:** supported RAW & raster formats, XMP interoperability & LRCAT catalog import into an Ember library. Originals remain separate from non-destructive edits.
- **Automation:** engine commands, CLI/MCP & native control/hidden QA. [Control protocol](docs/control-protocol.md) documents interfaces.

React/Tauri targets **macOS & Windows**. Existing egui, Linux, web/WASM & CLI hosts remain in source. Format support & implementation gaps are tracked in [ROADMAP](ROADMAP.md) & [parity tracker](docs/parity.md); feature availability is not proof of reference editor parity.

## Auto, masks & AI

Local Auto is deterministic & closed-loop ([docs/auto-tone.md](docs/auto-tone.md)); Auto Noise Reduction measures each photo's noise ([docs/noise-reduction.md](docs/noise-reduction.md)); look targets apply the statistics of sample photos to others ([docs/look-targets.md](docs/look-targets.md)). Culling source adds cancellable background analysis, paged review & explicit undoable acceptance. [Culling evaluation](docs/culling-evaluation.md) defines shoot-level labels, CLI scoring & proposed qualification targets. Face detection runs a user-downloaded YuNet model on Ember's own CPU ONNX interpreter (`crates/faces`, no extra runtime); a Candle-based YuNet exists only as a qualification example in `crates/segment`. Face/eye & similarity weights and [Personal Auto](docs/personal-auto-evaluation.md) training/evaluation remain unqualified; a learned [Auto decision model](docs/auto-decision-model.md) is planned. Denoise in the React/Tauri app is classical processing; the egui app & CLI build an optional AI Denoise path (`denoise` feature, on by default) that ships no model & offers no downloads ([docs/denoise.md](docs/denoise.md)). Optional [SAM 3 masking](docs/ai-masks.md) requires separately acquired model weights under Meta’s licence.

[OpenRouter photo assessment](crates/photo-ai/README.md) is an opt-in experiment for bounded, read-only model comparisons, available from the CLI. It produces proposals, does not apply edits or delete/reject photos, & has no validated provider winner. Credentials & real-photo evaluation are separate from this branding change.

[Local AI delivery](docs/local-ai-delivery.md) records current source work & remaining gates: local culling first, qualified face/eye/similarity weights next, then cloud comparison & Personal Auto. ChatGPT-plan OAuth remains deferred.

## Release status

This fork is under active development. Fork `main` includes merged [React/Tauri PR #1](https://github.com/Orthic-Labs/lightcraft/pull/1), [OpenRouter PR #2](https://github.com/Orthic-Labs/lightcraft/pull/2) & [Ember branding PR #3](https://github.com/Orthic-Labs/lightcraft/pull/3). No React/Tauri PR has been submitted to upstream LightCraft.

Source integration now targets upstream `25ef65c81eef81833a50ff5b61f8676d80611b06` (0.7.0); see [integration record](docs/upstream-integration-2026-10-12.md). Public builds, Rust tests & native qualification run through generated **RightKit GitHub Actions** (`.rightgit.json`). Local work is source/static-only. Only qualified green artifacts are promoted to desktop default, and none is qualified yet: the latest recorded hidden native QA runs failed on both targets ([CI restoration](docs/ci-restoration.md), [local AI delivery](docs/local-ai-delivery.md)). Source integration does not promote installed artifacts.

## Quick start

Rust workspace uses internal `lightcraft-*` crate/binary names for source & command compatibility. React source lives in `apps/lightcraft-desktop/web`; Tauri host lives in `apps/lightcraft-desktop`. RightKit versions are pinned by manifests & lockfiles; declarations alone do not establish integrated capabilities.

Install pinned frontend tooling & dependencies, then run local static checks:

```sh
corepack pnpm install --frozen-lockfile
pnpm typecheck
pnpm desktop:static
```

Use generated `ci` & `release-candidate` Actions lanes for compilation, eight Rust quality gates (fmt, clippy, heif, test, parity, layers, assets & WASM), frontend build & exact-artifact Mac/Windows hidden QA. [docs/releasing.md](docs/releasing.md) describes the upstream release pipeline, which runs only for `storytold/lightcraft`; the fork's candidate lane & its evidence are recorded in [CI restoration](docs/ci-restoration.md) & the [integration record](docs/upstream-integration-2026-10-12.md). [AGENTS.md](AGENTS.md) defines contribution & original-implementation rules.

## Documentation

- [Branding & existing library/preferences compatibility](docs/branding.md)
- [Fork strategy & selective upstream absorption](docs/fork-strategy.md)
- [LRCAT catalog import](docs/lightroom-catalog-import.md)
- [XMP interoperability](docs/xmp-interop.md)
- [Library shortcuts](docs/library-shortcuts.md)
- [MCP](docs/mcp.md) & [native control protocol](docs/control-protocol.md)
- [Contributor credits](docs/contributors.md)

## Licence & attribution

Original upstream code is Copyright (c) 2026 ArtCraft Team & LightCraft contributors. Ember fork changes & original flame artwork are by Orthic Labs. Preserve [NOTICE](NOTICE), contributor provenance & [asset attribution](assets/ATTRIBUTION.md).

Code is **MIT OR Apache-2.0**, except `crates/segment` & `crates/fetch`, which are **Apache-2.0 only**; SAM model weights have separate terms & are not distributed here. See [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE) & [NOTICE](NOTICE). Inter uses SIL OFL 1.1. ArtCraft product marks have been removed; Ember is independently maintained.
