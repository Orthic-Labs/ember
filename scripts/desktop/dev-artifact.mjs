import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fail, repoRoot, requireEnv } from './lib.mjs';

// Generated dev jobs have no release-candidate admission or matrix environment.
// Bind their actual checkout & native host without inventing candidate admission.
if (process.env.GITHUB_ACTIONS !== 'true') fail('dev artifacts require generated RightKit Actions');
const platform = process.platform === 'darwin' ? 'macos' : process.platform === 'win32' ? 'windows' : null;
if (!platform) fail('dev artifacts require a macOS or Windows runner');
const architecture = platform === 'macos' ? 'arm64' : 'x86_64';
if (process.arch !== (platform === 'macos' ? 'arm64' : 'x64')) fail('dev artifact runner architecture does not match native target');
const revision = requireEnv('GITHUB_SHA');
if (!/^[0-9a-f]{40}$/.test(revision)) fail('GITHUB_SHA must be an exact source revision');
const head = spawnSync('git', ['rev-parse', 'HEAD'], { cwd: repoRoot, encoding: 'utf8', windowsHide: true });
if (head.error || head.status !== 0 || head.stdout.trim() !== revision) fail('dev artifact checkout HEAD must equal GITHUB_SHA');
const env = {
  ...process.env,
  RIGHT_GIT_SOURCE_REVISION: revision,
  RIGHT_GIT_RELEASE_PLATFORM: platform,
  RIGHT_GIT_RELEASE_ARCHITECTURE: architecture,
  RIGHT_GIT_ARTIFACT_ROOT: path.join(requireEnv('RUNNER_TEMP'), `ember-dev-${platform}-${revision}`),
};
const build = spawnSync(process.execPath, [path.join(repoRoot, 'scripts/desktop/build-native.mjs'), 'dev-artifact'], { cwd: repoRoot, env, stdio: 'inherit', windowsHide: true });
if (build.error || build.status !== 0) fail(`native dev artifact build failed: ${build.error?.message || build.status}`);
