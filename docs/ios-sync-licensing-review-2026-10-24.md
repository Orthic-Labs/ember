# iOS, sync and licensing direction: review

Date: 2026-10-24. Commit reviewed: b4a444e. Read-only; nothing built or run. Tracking issue:
Orthic-Labs/ember#253. `plan/` is gitignored and was absent.

## Charter

Adrian's direction (2026-10-24): across the suite, Apple-device sync via CloudKit, Pro via StoreKit,
Windows/Android and cross-device over the Pulse mesh, everything managed from the phone through
Pulse. Ember waits on the associated-domains work: no login, no links. This review checks what the
iOS bridge, the catalog and the licensing wiring have against that direction. Companion reviews of
the same date live in each product repo.

## What is true today

| Axis | State | Evidence |
|---|---|---|
| iOS app | `crates/mobile` is a UniFFI 0.32 `EmberSession` over `DesktopHandle` (`open/run/snapshot/view_slice/preview/preview_quick/preview_bytes/release_preview/preferences/persist/close`, `BRIDGE_VERSION=1`, three error variants, `staticlib`, no hand-written `unsafe`). The Swift app has a library chooser (folder bookmark or demo), grid, rate and flag, Light/Color/Effects/Vignette sliders from Rust control specs, one undo step per gesture, import into `Originals`, JPEG export and share; Settings says licensing is not configured. Build is Mac-only via `scripts/ios/build-xcframework.sh`; outputs gitignored. CI runs the Rust bridge tests but no xcodebuild; `right-release.config.mjs` has only win and mac targets; TestFlight not run; tracking #212. No `.entitlements` file although the App Group is configured in Swift. | `crates/mobile/src/lib.rs:17-42,65-69,88-200,210-281`, `crates/mobile/Cargo.toml:11,25-26,35`, `ios/Ember/LibraryStore.swift:136-141,177,191-225,292-345`, `ios/Ember/SettingsView.swift:14`, `ios/.gitignore:5-6`, `xtask/src/main.rs:317`, `right-release.config.mjs:39-69`, `docs/ios-architecture.md:16-18`, `ios/Ember/OwnerWiring.swift:22,36` |
| Sync | Nothing built. The catalog is a local journal plus snapshot with local `u64` ids and an exclusive lock that refuses a second opener, including two computers on a shared folder, so an iCloud Drive library is unsafe by design. The only portable path is XMP sidecars. `docs/ios-architecture.md:54-84` plans portable UUID entities, CloudKit private DB via RightKit sync, immutable revisions and conflict branches, and Windows "portable transfer bundles" (`:106`). `ROADMAP.md:39` says no cloud sync; `docs/parity-checklist.md:497,549` mark LR-CLOUD-SYNC and MENU-APP-SYNC out of scope; `docs/gaps.md` has no sync or iOS entry. | `crates/catalog/src/lib.rs:3-7`, `crates/catalog/src/model.rs:10`, `crates/catalog/src/lock.rs:1-7`, `docs/xmp-interop.md:12-14` |
| Entitlement | No StoreKit or SellRight code. iOS wires `RightKitLicense` and `RightKitStore` with a `LicenseController` over Keychain, fail-closed when `EMBER_LICENSE_BASE_URL` is empty. `rightkit-mandatory.json` M12/M13 declare no paid features, activation or signer; `docs/ios-architecture.md:96-104` defers pricing to RightKitPurchases. Workspace licence MIT or Apache-2.0. | `ios/project.yml:36,49-56`, `ios/Ember/OwnerWiring.swift:37-53`, `rightkit-mandatory.json`, `Cargo.toml:8` |
| Pulse and mesh | No iroh, rightkit-mesh or Pulse code. The control channel is loopback with a per-launch owner-only token file; MCP connect mode layers on it; M6/M7 assert no daemon or service. No phone path exists. | `docs/control-protocol.md:3-4,25-61`, `docs/mcp.md:14,25-27`, `rightkit-mandatory.json` |
| Robustness | Input caps and NUL and JSON checks in the bridge; panics contained per request by `guard::catch` and `catch_unwind`; `panic = "unwind"` kept. Gap: `guard::install_hook` is called only in the desktop binary, so contained panics in the mobile crate are never recorded and Swift's `JSONLDiagnosticStore` receives nothing from Rust. Closed-state detection is a substring match. | `crates/mobile/src/lib.rs:26-28,53,98,115,181-200`, `crates/desktop-host/src/controller.rs:191,350,363,378,472-484`, `crates/engine/src/guard.rs:31-46`, `apps/lightcraft/src/main.rs:737`, `Cargo.toml:154`, `ios/Ember/OwnerWiring.swift:35` |
| Docs | `ROADMAP.md` has no iOS milestone, its log ends 2026-10-10, no Last updated line, `:39` cites "tablet companion (#74)" but #74 is the LR-VID-TRIM parity issue, `:42` lists only `crates/sysmem` for unsafe while `CLAUDE.md:42-48` lists sysmem, av1dec and mobile. `AGENTS.md:42,61` is stale against `CLAUDE.md`. `docs/ios-architecture.md:22-24` says V1 continues on the Mac through iCloud sync while `:18` lists it as not yet. | as cited |

## Findings, ranked

1. **Sync direction is undefined in the tracker.** The parity rows say out of scope, the iOS plan
   assumes CloudKit, the owner's direction is CloudKit plus mesh, and the catalog lock forbids
   shared folders. Agents pick work from `docs/gaps.md`, so nobody will start it. Do: a ranked gap
   entry and a ROADMAP milestone; flip LR-CLOUD-SYNC from out of scope; first step is portable
   UUIDs (`docs/ios-architecture.md:58`).
2. **Licensing contradiction.** M12/M13 "no paid features" versus the RightKitLicense wiring and the
   Pro intent. Do: re-ratify the rulings before any StoreKit work through RightKitPurchases (held
   in RightKitSwift 0.9.0, `ios/project.yml:7`).
3. **iOS panics are silent.** Do: install `guard::install_hook` or a diagnostics callback in
   `EmberSession::open`, with a test.
4. **iOS has no CI or release lane.** Do: an XCFramework build on the macos-15 lane; AppIcon and
   its `assets/ATTRIBUTION.md` entry (`docs/ios-architecture.md:16`).
5. **Missing entitlements file** for the App Group now, iCloud and Associated Domains later.
6. **Doc drift** as listed above.
7. **Phone-to-Mac control needs a new transport.** The loopback token channel cannot be reused and
   M6/M7 forbid a daemon. Design note only; the mesh is the candidate.

## Decisions needed

- Record the sync direction as a gap and milestone, with the progress-doc header bumps
  `CLAUDE.md` requires.
- Re-ratify M12/M13 against the StoreKit Pro intent.

## Related

Orthic-Labs/ember#253 (tracking), #212, #240; bogusyogi/pulse#478. Companion reviews of 2026-10-24
in heardright, coderight, scraperight, sellright, pulse, genright, postright and CutRight.
