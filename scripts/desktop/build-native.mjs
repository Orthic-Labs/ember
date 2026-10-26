import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, readdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { runCargoSync } from '@rightkit/release/managed-cargo.mjs';
import { resolveTauriBundleDirectory } from '@rightkit/release/development.mjs';
import { assertNoForbiddenBuildFeatures } from '@rightkit/release/qa-build-features.mjs';
import { artifactRoot, cargoCompilerArtifacts, currentPlatform, fail, materializeArtifacts, repoRoot, requireFile, sha256 } from './lib.mjs';
import { requireDoor } from '@rightkit/release/door.mjs';
// Runs only as a step right-release runs inside a rightkit-gate --to stage (class A first); by hand it is refused.
requireDoor('scripts/desktop/build-native.mjs');

if (process.env.GITHUB_ACTIONS !== 'true') fail('native build requires generated RightKit Actions');
const mode = process.argv[2] ?? 'candidate';
if (!['candidate', 'dev-artifact'].includes(mode)) fail(`unknown native build mode: ${mode}`);
const devArtifact = mode === 'dev-artifact';
const features = devArtifact ? 'custom-protocol' : 'qa-native,custom-protocol';
const platform = currentPlatform();
if ((platform === 'macos' && process.platform !== 'darwin') || (platform === 'windows' && process.platform !== 'win32')) fail('candidate requires target-native runner');
const target = platform === 'macos' ? 'aarch64-apple-darwin' : 'x86_64-pc-windows-msvc';
const guiCargoArgs = ['build', '--locked', '--manifest-path', 'apps/lightcraft-desktop/Cargo.toml', '--features', features, '--target', target, '--message-format=json'];
const tauriArgs = ['exec', 'tauri', 'build', '--ci', '--no-sign', '--debug', '--config', 'tauri.candidate.conf.json', '--config', 'tauri.worker.conf.json', '--target', target, '--features', features, '--bundles', platform === 'macos' ? (devArtifact ? 'app' : 'app,dmg') : 'nsis', '--', '--locked'];
const root = path.resolve(artifactRoot());
mkdirSync(root, { recursive: true });
function run(args, cwd = repoRoot, env = process.env) {
  const result = spawnSync('pnpm', args, { cwd, env, stdio: 'inherit', windowsHide: true, shell: process.platform === 'win32' });
  if (result.error || result.status !== 0) fail(`pnpm ${args[0]} failed: ${result.error?.message || result.status}`);
}
function printCompilerErrors(stdout) {
  for (const line of (stdout || '').split('\n')) {
    let entry;
    try { entry = JSON.parse(line); } catch { continue; }
    if (entry.reason === 'compiler-message' && entry.message?.level === 'error') {
      process.stderr.write(entry.message.rendered || entry.message.message + '\n');
    }
  }
}
// Existing engine embeds licensed craft-fonts; every candidate supplies pinned input.
const fonts = path.join(process.env.RUNNER_TEMP, 'lightcraft-build-fonts');
const clone = spawnSync('git', ['clone', '--quiet', 'https://github.com/storytold/craft-fonts.git', fonts], { stdio: 'inherit', windowsHide: true });
if (clone.error || clone.status !== 0) fail('craft-fonts clone failed');
const checkout = spawnSync('git', ['-C', fonts, 'checkout', '--quiet', '8dcdacd5153e64560d109541a47d806f26f048c0'], { stdio: 'inherit', windowsHide: true });
if (checkout.error || checkout.status !== 0) fail('pinned craft-fonts checkout failed');
const env = { ...process.env, CRAFT_FONTS_DIR: fonts, CRAFT_FONTS_REQUIRED: '1' };
if (devArtifact) {
  // Check requested/default Cargo features, keeping normal development-profile
  // Tauri behavior. Release-only binary hardening does not apply to this lane.
  assertNoForbiddenBuildFeatures({
    root: repoRoot,
    cargoManifest: 'apps/lightcraft-desktop/Cargo.toml',
    commands: [
      { label: 'dev GUI Cargo', command: { cmd: 'cargo', args: guiCargoArgs, env } },
      { label: 'dev GUI Tauri', command: { cmd: 'pnpm', args: tauriArgs, env } },
    ],
    tier: 'development artifact',
  });
}
run(['run', 'build'], repoRoot, env);
// Retain headless experiment CLI as an exact-revision compiler artifact too.
const cliCompiler = runCargoSync(['build', '--locked', '-p', 'lightcraft-cli', '-p', 'lightcraft-av1dec', '--target', target, '--message-format=json'], { cwd: repoRoot, env, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, windowsHide: true });
writeFileSync(path.join(root, 'cli-cargo-artifacts.jsonl'), cliCompiler.stdout || '');
if (cliCompiler.stderr) process.stderr.write(cliCompiler.stderr);
if (cliCompiler.error || cliCompiler.status !== 0) {
  printCompilerErrors(cliCompiler.stdout);
  fail(`RightKit headless CLI Cargo failed: ${cliCompiler.error?.message || cliCompiler.status}`);
}
// Preserve offline extractor independently of later desktop packaging/installation.
const cliRecord = path.join(root, 'cli-cargo-artifacts.jsonl');
const cliEntries = cargoCompilerArtifacts(cliRecord).filter(entry => (entry.target === 'lightcraft-cli' || entry.target === 'ember-av1-worker') && entry.kind === 'executable');
if (cliEntries.length !== 2) fail('expected the headless CLI and the AV1 worker compiler artifacts');
materializeArtifacts(cliEntries, path.join(root, 'headless-cli'), {
  sourceRevision: process.env.RIGHT_GIT_SOURCE_REVISION,
  platform,
  architecture: platform === 'macos' ? 'arm64' : 'x86_64',
  compilerRecordSha256: sha256(cliRecord),
  qualification: 'compiled; source gates required before corpus extraction',
  unsigned: true,
});
const compiler = runCargoSync(guiCargoArgs, { cwd: repoRoot, env, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, windowsHide: true });
// Retain compiler records on failures too. A large asynchronous stdout write
// followed by process failure can drop diagnostics near the end of JSON output.
writeFileSync(path.join(root, 'cargo-artifacts.jsonl'), `${cliCompiler.stdout || ''}\n${compiler.stdout || ''}`);
if (compiler.stderr) process.stderr.write(compiler.stderr);
if (compiler.error || compiler.status !== 0) {
  printCompilerErrors(compiler.stdout);
  fail(`RightKit candidate Cargo failed: ${compiler.error?.message || compiler.status}`);
}
const nativeRoot = path.join(repoRoot, 'apps/lightcraft-desktop');
// The AV1 decoder worker ships beside the desktop executable: Tauri's externalBin (tauri.worker.conf.json) takes it from
// binaries/ember-av1-worker-<target-triple>[.exe] and installs it next to the app under its plain name.
const workerEntry = cliEntries.find(entry => entry.target === 'ember-av1-worker');
mkdirSync(path.join(nativeRoot, 'binaries'), { recursive: true });
cpSync(workerEntry.path, path.join(nativeRoot, 'binaries', `ember-av1-worker-${target}${platform === 'macos' ? '' : '.exe'}`));
if (devArtifact) {
  const guiEntries = cargoCompilerArtifacts(path.join(root, 'cargo-artifacts.jsonl')).filter(entry => entry.target === 'lightcraft-desktop' && entry.kind === 'executable');
  if (guiEntries.length !== 1) fail('expected exactly one desktop GUI compiler artifact');
  for (const entry of [guiEntries[0], workerEntry]) requireFile(entry.path, `dev artifact ${entry.target}`);
}
// Candidates are unsigned: tauri.candidate.conf.json turns off updater artifacts (they need the suite minisign key, which right-release sign-updater applies later, never in this build).
run(tauriArgs, nativeRoot, env);
// Cargo's dev profile writes to target/<triple>/debug; @rightkit/release before 0.2.167 still looks under release-iterate.
const descriptor = { productName: 'Ember', targetTriple: target, manifestPath: 'apps/lightcraft-desktop/Cargo.toml', bundleDirectory: path.join('target', target, 'debug', 'bundle', platform === 'macos' ? 'macos' : 'nsis') };
const bundle = resolveTauriBundleDirectory(descriptor, { root: repoRoot, platform: platform === 'macos' ? 'mac' : 'win', env });
if (devArtifact) {
  const binaryRoot = platform === 'macos' ? path.join(bundle.installer, 'Contents', 'MacOS') : path.join(repoRoot, 'target', target, 'debug');
  const binaries = [];
  for (const name of ['lightcraft-desktop', 'ember-av1-worker']) {
    const filename = `${name}${platform === 'macos' ? '' : '.exe'}`;
    const file = path.join(binaryRoot, filename);
    requireFile(file, `packaged dev artifact ${name}`);
    binaries.push({ filename, sha256: sha256(file) });
  }
  // Tauri has already applied its Windows bundle marker. Mac signing happens
  // later in RightGit, so these Mac hashes identify unsigned build inputs only.
  const evidenceRoot = platform === 'macos' ? path.join(bundle.installer, 'Contents', 'Resources') : bundle.bundleRoot;
  mkdirSync(evidenceRoot, { recursive: true });
  writeFileSync(path.join(evidenceRoot, 'ember-dev-artifact.json'), JSON.stringify({
    schema: 1,
    sourceRevision: process.env.RIGHT_GIT_SOURCE_REVISION,
    platform,
    architecture: platform === 'macos' ? 'arm64' : 'x86_64',
    binaryHashStage: platform === 'macos' ? 'before-developer-id-signing' : 'after-tauri-bundling',
    qaNativeFeature: false,
    buildFeatureCheck: 'passed',
    binaries,
  }, null, 2) + '\n');
}
const destination = path.join(root, 'packages', path.basename(bundle.installer));
mkdirSync(path.dirname(destination), { recursive: true });
cpSync(bundle.installer, destination, { recursive: true });
writeFileSync(path.join(root, 'bundle-record.json'), JSON.stringify({ schema: 1, path: path.relative(root, destination).replaceAll('\\', '/'), kind: platform === 'macos' ? 'app' : 'installer', sourceRevision: process.env.RIGHT_GIT_SOURCE_REVISION }, null, 2) + '\n');

if (platform === 'macos' && !devArtifact) {
  const dmgRoot = path.join(path.dirname(bundle.bundleRoot), 'dmg');
  const dmgs = readdirSync(dmgRoot).filter(name => name.endsWith('.dmg'));
  if (dmgs.length !== 1) fail('candidate must produce exactly one DMG');
  const dmg = path.join(dmgRoot, dmgs[0]);
  cpSync(dmg, path.join(root, 'packages', dmgs[0]));
}
const licenses = path.join(root, 'licenses');
mkdirSync(licenses, { recursive: true });
cpSync(path.join(repoRoot, 'assets/fonts/OFL-Inter.txt'), path.join(licenses, 'OFL-Inter.txt'));
for (const name of ['ATTRIBUTION.md', 'LICENSE-APACHE', 'LICENSE-MIT']) {
  const source = name === 'ATTRIBUTION.md' ? path.join(repoRoot, 'assets', name) : path.join(repoRoot, name);
  cpSync(source, path.join(licenses, name));
}
cpSync(path.join(repoRoot, 'NOTICE'), path.join(licenses, 'NOTICE'));
const pytorchLicense = path.join(licenses, 'docs', 'licenses', 'pytorch-bicubic-LICENSE.txt');
mkdirSync(path.dirname(pytorchLicense), { recursive: true });
cpSync(path.join(repoRoot, 'docs', 'licenses', 'pytorch-bicubic-LICENSE.txt'), pytorchLicense);
for (const dir of readdirSync(fonts, { withFileTypes: true }).filter(entry => entry.isDirectory())) {
  const fontDir = path.join(fonts, dir.name);
  const ofls = readdirSync(fontDir).filter(name => /OFL|LICENSE/i.test(name));
  for (const name of ofls) cpSync(path.join(fontDir, name), path.join(licenses, `${dir.name}-${name}`));
}
