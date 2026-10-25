import { readFileSync } from 'node:fs';
import path from 'node:path';
const build = { cmd: 'node', args: ['scripts/desktop/build-native.mjs'] };
// Cargo's dev profile writes bundles to target/<triple>/debug; @rightkit/release before 0.2.167 looks under release-iterate.
// The workspace version is the product version (Cargo.toml [workspace.package]); package.json and tauri.conf.json match it.
const version = /^\[workspace\.package\][^[]*?^version\s*=\s*"(\d+\.\d+\.\d+)"/ms.exec(readFileSync(new URL('./Cargo.toml', import.meta.url), 'utf8'))?.[1];
if (!version) throw new Error('Cargo.toml [workspace.package] must declare a SemVer version');
// Signed candidates are the exact unsigned bytes from the candidate stage (LEGION_UNSIGNED_CANDIDATE_ROOT, set by release-candidate.yml);
// scripts/desktop/finalize-candidate.mjs only stages and archives them. RightKit signs, notarizes, hardens and seals.
const stageCommand = { cmd: 'node', args: ['scripts/desktop/finalize-candidate.mjs', 'stage'] };
const packageCommand = { cmd: 'node', args: ['scripts/desktop/finalize-candidate.mjs', 'package'], livenessOwnerTimeoutMs: 20 * 60 * 1000 };
const winStage = `dist/native/windows-x86_64/ember-${version}`;
const macStage = `dist/native/macos-arm64/ember-${version}`;
const winArchive = `dist/releases/windows/${version}/x86_64/ember-${version}-windows-x86_64.zip`;
// Tauri updater bundles (rightkit#237 GitHub Releases lane): the NSIS setup exe and the macOS .app.tar.gz, each with its .sig made by
// right-release sign-updater with the suite key. Names stay within [A-Za-z0-9._+-] so GitHub keeps them. They come from an updater-enabled
// Tauri bundle (tauri.conf.json createUpdaterArtifacts); candidate bundles turn that off (tauri.candidate.conf.json). Portable zips stay in `artifacts`.
const winUpdater = `target/x86_64-pc-windows-msvc/debug/bundle/nsis/Ember_${version}_x64-setup.exe`;
const macUpdater = 'target/aarch64-apple-darwin/debug/bundle/macos/Ember.app.tar.gz';
const macArchive = `dist/releases/mac/${version}/arm64/ember-${version}-macos-arm64.zip`;
// The updater public key is the one Tauri embeds (apps/lightcraft-desktop/tauri.conf.json plugins.updater.pubkey); release verifies each
// updater signature against it before sealing, so the config reads it from that file instead of keeping a second copy.
const updaterPublicKey = JSON.parse(readFileSync(new URL('./apps/lightcraft-desktop/tauri.conf.json', import.meta.url), 'utf8'))?.plugins?.updater?.pubkey;
if (typeof updaterPublicKey !== 'string' || !updaterPublicKey.trim()) throw new Error('apps/lightcraft-desktop/tauri.conf.json plugins.updater.pubkey must be set');
export default {
  schema: 1,
  app: 'lightcraft-desktop',
  version,
  hostedWorkflows: 'right-git-ci-only',
  distribution: { provider: 'github-releases', repository: 'Orthic-Labs/ember', updaterKeyId: '29395F9FF466261D', updaterPublicKey },
  // The tool name release runs; the version is package.json packageManager (pnpm@11.25.0).
  packageManager: 'pnpm',
  nativeAssembly: { cargoManifest: 'apps/lightcraft-desktop/Cargo.toml' },
  // M21/M23 release-switch scan (switches.json lists every environment read the shipped code does; directories below never ship).
  releaseSwitches: {
    nonShipped: [
      { dir: 'xtask', reason: 'workspace CI tooling (cargo xtask: ci, layers, assets, parity, wasm); not a dependency of lightcraft-desktop or lightcraft-cli, never bundled' },
      { dir: 'tools', reason: 'reference, installer and training scripts (Python, shell, PowerShell); not compiled, not bundled' },
      { dir: 'packaging', reason: 'packaging scripts and icon renderers for the egui, Linux, FreeBSD and web hosts; the Tauri bundle is built by right-release; not bundled' },
      { dir: 'ios', reason: 'SwiftUI app of the iOS target; no desktop release target builds or bundles it' },
      { dir: 'crates/mobile', reason: 'UniFFI bridge of the iOS target; no crate of the desktop or CLI trees depends on it' },
      { dir: 'crates/denoise-train', reason: 'denoise training tooling; nothing depends on it' },
      { dir: 'apps/lightcraft', reason: 'egui desktop host; not part of the signed bundle (bin/lightcraft-desktop, bin/lightcraft-cli, bin/ember-av1-worker)' },
      { dir: 'apps/lightcraft-web', reason: 'WASM build of the egui UI; packaged separately, not part of the desktop bundle' },
    ],
  },
  buildInputs: {
    include: ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'crates/**', 'apps/**', 'assets/**', 'scripts/desktop/**', 'package.json', 'pnpm-lock.yaml', 'pnpm-workspace.yaml', 'right-release.config.mjs'],
    exclude: ['**/node_modules/**', 'apps/lightcraft-desktop/web/dist/**'],
  },
  development: {
    targets: {
      mac: { build: [build], install: { kind: 'tauri', productName: 'Ember', targetTriple: 'aarch64-apple-darwin', bundleDirectory: 'target/aarch64-apple-darwin/debug/bundle/macos', manifestPath: 'apps/lightcraft-desktop/Cargo.toml', destination: '/Applications/Ember.app' } },
      win: { build: [build], install: { kind: 'tauri', productName: 'Ember', targetTriple: 'x86_64-pc-windows-msvc', bundleDirectory: 'target/x86_64-pc-windows-msvc/debug/bundle/nsis', manifestPath: 'apps/lightcraft-desktop/Cargo.toml', expectedInstalledPath: path.join(process.env.LOCALAPPDATA || '.', 'Ember', 'lightcraft-desktop.exe') } },
    },
  },
  targets: {
    win: {
      signed: true,
      signingContract: 'windows-raw-exe-authenticode-before-portable-v1',
      packageKind: 'portable-zip',
      architecture: 'x86_64',
      targetTriple: 'x86_64-pc-windows-msvc',
      publishBlocked: 'signed release candidate only; publication is a separate admitted stage',
      prePackage: stageCommand,
      sign: {
        prePackageFiles: [`${winStage}/bin/lightcraft-desktop.exe`, `${winStage}/bin/lightcraft-cli.exe`, `${winStage}/bin/ember-av1-worker.exe`],
        receipt: '.right-release/receipts/windows-x86_64-raw-exe.json',
      },
      package: packageCommand,
      artifacts: [winArchive],
      updater: { artifacts: [{ file: winUpdater, signature: `${winUpdater}.sig`, platform: 'windows-x86_64' }] },
    },
    mac: {
      signed: true,
      signingContract: 'macos-developer-id-notarized-portable-v1',
      packageKind: 'portable-zip',
      architecture: 'arm64',
      targetTriple: 'aarch64-apple-darwin',
      publishBlocked: 'signed release candidate only; publication is a separate admitted stage',
      prePackage: stageCommand,
      sign: {
        prePackageFiles: [`${macStage}/bin/lightcraft-desktop`, `${macStage}/bin/lightcraft-cli`, `${macStage}/bin/ember-av1-worker`],
        receipt: '.right-release/receipts/macos-arm64-signing.json',
      },
      package: packageCommand,
      notarize: { file: macArchive, staple: false, receipt: '.right-release/receipts/macos-arm64-notarization.json' },
      artifacts: [macArchive],
      updater: { artifacts: [{ file: macUpdater, signature: `${macUpdater}.sig`, platform: 'darwin-aarch64' }] },
    },
  },
};
