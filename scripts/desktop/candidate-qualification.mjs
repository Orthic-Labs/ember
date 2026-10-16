import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { runCargoSync } from '@rightkit/release/managed-cargo.mjs';
import { fail, loadReleaseConfig, repoRoot } from './lib.mjs';
import path from 'node:path';

function runLsappinfo(args) {
  const result = spawnSync('/usr/bin/lsappinfo', args, {
    encoding: 'utf8',
    windowsHide: true,
  });
  return {
    command: ['lsappinfo', ...args],
    status: result.status,
    signal: result.signal,
    stdout: result.stdout || '',
    stderr: result.stderr || '',
    error: result.error?.message || null,
  };
}

function lsappinfoValue(stdout) {
  const equals = stdout.indexOf('=');
  if (equals < 0) return null;
  const value = stdout.slice(equals + 1).trim();
  if (value.startsWith('"')) {
    const end = value.indexOf('"', 1);
    return end > 0 ? value.slice(1, end) : null;
  }
  return value.split(/\s+/)[0] || null;
}

function collectMacForegroundBaseline(root, record) {
  const front = runLsappinfo(['front']);
  const asn = front.stdout.trim().replace(/:$/, '');
  const pid = asn ? runLsappinfo(['info', '-only', 'pid', asn]) : null;
  const name = asn ? runLsappinfo(['info', '-only', 'name', asn]) : null;
  const pidValue = lsappinfoValue(pid?.stdout || '');
  const nameValue = lsappinfoValue(name?.stdout || '');
  const parsedPid = pidValue && /^\d+$/.test(pidValue) ? Number(pidValue) : null;
  const report = {
    schema: 1,
    sourceRevision: record.sourceRevision,
    platform: record.platform,
    architecture: record.architecture,
    readOnly: true,
    asn: asn || null,
    parsed: { pid: parsedPid, name: nameValue },
    commands: { front, pid, name },
  };
  const output = path.join(root, 'mac-foreground-baseline.json');
  writeFileSync(output, JSON.stringify(report, null, 2) + '\n');
  console.log(`[candidate] Mac foreground baseline: asn=${report.asn || 'unavailable'} pid=${report.parsed.pid || 'unavailable'} name=${report.parsed.name || 'unavailable'} diagnostic=${output}`);
}

export async function qualify(record, root) {
  if (process.env.GITHUB_ACTIONS !== 'true') fail('candidate qualification requires generated Actions');
  const executable = record.artifacts.find(artifact => artifact.target === 'lightcraft-desktop' && artifact.kind === 'executable');
  if (!executable) fail('candidate lacks Ember desktop executable');
  const { config } = await loadReleaseConfig();
  const targetName = record.platform === 'macos' ? 'mac' : 'win';
  const target = config.development.targets[targetName];
  const installed = record.platform === 'macos' ? path.join(target.install.destination, 'Contents/MacOS/lightcraft-desktop') : target.install.expectedInstalledPath;
  const installation = JSON.parse(readFileSync(path.join(root, 'installed-candidate.json'), 'utf8'));
  if (installation.schema !== 1 || installation.sourceRevision !== record.sourceRevision || installation.platform !== record.platform || installation.architecture !== record.architecture || installation.path !== installed) fail('installed candidate identity does not match qualified source & target');
  const installedHash = createHash('sha256').update(readFileSync(installed)).digest('hex');
  if (installation.sha256 !== installedHash) fail('installed candidate changed after Right Release installation');
  if (record.platform === 'macos') collectMacForegroundBaseline(root, record);
  const env = { ...process.env, CRAFT_FONTS_DIR: path.join(process.env.RUNNER_TEMP, 'lightcraft-build-fonts'), CRAFT_FONTS_REQUIRED: '1', RIGHTKIT_QA_HIDDEN: '1', RIGHTKIT_QA_UI_BINARY: installed, RIGHTKIT_QA_EVIDENCE: path.join(root, 'qa-evidence'), RIGHTKIT_QA_SOURCE_REVISION: record.sourceRevision, RIGHTKIT_QA_ARCHITECTURE: record.architecture, RIGHTKIT_QA_INSTALLED_ARTIFACT_SHA256: installedHash };
  // Match candidate target/features (both on Cargo's dev profile) so qualification reuses
  // the native engine compilation.
  const result = runCargoSync(['test', '--locked', '-p', 'lightcraft-desktop', '--test', 'native_qualification', '--target', target.install.targetTriple, '--features', 'qa-native,custom-protocol', '--', '--ignored', '--nocapture'], { cwd: repoRoot, env, stdio: 'inherit', windowsHide: true });
  if (result.error || result.status !== 0) fail(`native qualification failed: ${result.error?.message || result.status}`);
  if (record.platform === 'macos') {
    // GitHub directory artifacts lose Unix modes. Retain the exact installed,
    // ad-hoc signed bundle exercised above in Apple's resource-preserving ZIP.
    const archive = path.join(root, 'packages', 'Ember-macos-arm64-qualified.zip');
    mkdirSync(path.dirname(archive), { recursive: true });
    const packed = spawnSync('/usr/bin/ditto', ['-c', '-k', '--sequesterRsrc', '--keepParent', target.install.destination, archive], { stdio: 'inherit' });
    if (packed.error || packed.status !== 0) fail(`qualified Mac bundle archive failed: ${packed.error?.message || packed.status}`);
    const archiveHash = createHash('sha256').update(readFileSync(archive)).digest('hex');
    writeFileSync(path.join(root, 'qualified-bundle.json'), JSON.stringify({ schema: 1, sourceRevision: record.sourceRevision, platform: record.platform, architecture: record.architecture, path: path.relative(root, archive), sha256: archiveHash, installedExecutableSha256: installedHash, nativeQualification: 'passed' }, null, 2) + '\n');
  }
}
