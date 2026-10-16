import { readFileSync } from 'node:fs';
import path from 'node:path';
const build = { cmd: 'node', args: ['scripts/desktop/build-native.mjs'] };
// The workspace version is the product version (Cargo.toml [workspace.package]); package.json and tauri.conf.json carry a stale 0.2.1.
const version = /^\[workspace\.package\][^[]*?^version\s*=\s*"(\d+\.\d+\.\d+)"/ms.exec(readFileSync(new URL('./Cargo.toml', import.meta.url), 'utf8'))?.[1];
if (!version) throw new Error('Cargo.toml [workspace.package] must declare a SemVer version');
// Signed candidates are the exact unsigned bytes from the candidate stage (LEGION_UNSIGNED_CANDIDATE_ROOT, set by release-candidate.yml);
// scripts/desktop/finalize-candidate.mjs only stages and archives them. RightKit signs, notarizes, hardens and seals.
const stageCommand = { cmd: 'node', args: ['scripts/desktop/finalize-candidate.mjs', 'stage'] };
const packageCommand = { cmd: 'node', args: ['scripts/desktop/finalize-candidate.mjs', 'package'] };
const winStage = `dist/native/windows-x86_64/ember-${version}`;
const macStage = `dist/native/macos-arm64/ember-${version}`;
const winArchive = `dist/releases/windows/${version}/x86_64/ember-${version}-windows-x86_64.zip`;
const macArchive = `dist/releases/mac/${version}/arm64/ember-${version}-macos-arm64.zip`;
export default {
  schema: 1,
  app: 'lightcraft-desktop',
  version,
  hostedWorkflows: 'right-git-ci-only',
  distribution: { provider: 'github-releases', repository: 'Orthic-Labs/ember' },
  packageManager: 'pnpm@11.24.0',
  nativeAssembly: { cargoManifest: 'apps/lightcraft-desktop/Cargo.toml' },
  buildInputs: {
    include: ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'crates/**', 'apps/**', 'assets/**', 'scripts/desktop/**', 'package.json', 'pnpm-lock.yaml', 'pnpm-workspace.yaml', 'right-release.config.mjs'],
    exclude: ['**/node_modules/**', 'apps/lightcraft-desktop/web/dist/**'],
  },
  development: {
    targets: {
      mac: { build: [build], install: { kind: 'tauri', productName: 'Ember', targetTriple: 'aarch64-apple-darwin', manifestPath: 'apps/lightcraft-desktop/Cargo.toml', destination: '/Applications/Ember.app' } },
      win: { build: [build], install: { kind: 'tauri', productName: 'Ember', targetTriple: 'x86_64-pc-windows-msvc', manifestPath: 'apps/lightcraft-desktop/Cargo.toml', expectedInstalledPath: path.join(process.env.LOCALAPPDATA || '.', 'Ember', 'lightcraft-desktop.exe') } },
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
        prePackageFiles: [`${winStage}/bin/lightcraft-desktop.exe`, `${winStage}/bin/lightcraft-cli.exe`],
        receipt: '.right-release/receipts/windows-x86_64-raw-exe.json',
      },
      package: packageCommand,
      artifacts: [winArchive],
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
        prePackageFiles: [`${macStage}/bin/lightcraft-desktop`, `${macStage}/bin/lightcraft-cli`],
        receipt: '.right-release/receipts/macos-arm64-signing.json',
      },
      package: packageCommand,
      notarize: { file: macArchive, receipt: '.right-release/receipts/macos-arm64-notarization.json' },
      artifacts: [macArchive],
    },
  },
};
