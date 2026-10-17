import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import path from "node:path";
import { runDevelopment } from "@rightkit/release/development.mjs";
import {
  artifactRoot,
  cargoCompilerArtifacts,
  currentPlatform,
  fail,
  loadReleaseConfig,
  materializeArtifacts,
  repoRoot,
  safeRelative,
  sha256,
  sourceIdentity,
} from "./lib.mjs";
import { runBaseline } from "./denoise-baseline.mjs";

const mode = process.argv[2];
if (!new Set(["build", "check"]).has(mode)) fail("usage: node scripts/desktop/candidate.mjs <build|check>");
if (process.env.GITHUB_ACTIONS !== "true") fail("candidate build/check is GitHub Actions-only");
const platform = currentPlatform();
const root = path.resolve(artifactRoot());
const revision = sourceIdentity();
const architecture = process.env.RIGHT_GIT_RELEASE_ARCHITECTURE || process.env.RIGHT_GIT_TARGET_ARCH;
if (!architecture) fail("candidate requires RIGHT_GIT_RELEASE_ARCHITECTURE from generated target matrix");

function ensureReleaseCheckoutAttached() {
  const head = spawnSync("git", ["rev-parse", "HEAD"], { cwd: repoRoot, encoding: "utf8", windowsHide: true });
  if (head.error || head.status !== 0 || head.stdout.trim() !== revision) {
    fail(`candidate checkout HEAD must equal admitted source revision ${revision}`);
  }
  const branch = spawnSync("git", ["symbolic-ref", "--quiet", "HEAD"], { cwd: repoRoot, encoding: "utf8", windowsHide: true });
  if (branch.status === 0) return;
  const name = `right-release/${revision}`;
  const attached = spawnSync("git", ["switch", "--create", name, revision], { cwd: repoRoot, encoding: "utf8", windowsHide: true });
  if (attached.error || attached.status !== 0) {
    fail(`candidate checkout must attach admitted source revision to a branch: ${attached.error?.message || attached.stderr || attached.stdout || attached.status}`);
  }
  const attachedHead = spawnSync("git", ["rev-parse", "HEAD"], { cwd: repoRoot, encoding: "utf8", windowsHide: true });
  if (attachedHead.error || attachedHead.status !== 0 || attachedHead.stdout.trim() !== revision) {
    fail(`branch attachment changed candidate HEAD away from admitted source revision ${revision}`);
  }
  console.log(`desktop candidate checkout: attached ${name} at ${revision}`);
}

ensureReleaseCheckoutAttached();
const releaseTargetName = platform === "macos" ? "mac" : "win";
const { config } = await loadReleaseConfig();
if (config.schema !== 1 || !config.development?.targets?.[releaseTargetName]) {
  fail(`RightKit config requires schema: 1 development.targets.${releaseTargetName}`);
}

function configuredRecord() {
  const value = process.env.RIGHT_GIT_COMPILER_ARTIFACT_RECORD;
  if (!value) return path.join(root, "cargo-artifacts.jsonl");
  return path.isAbsolute(value) ? value : path.resolve(repoRoot, value);
}

function candidateRecord() {
  const file = path.join(root, "compiler-artifacts.json");
  if (!existsSync(file)) fail(`candidate compiler-artifacts.json is missing: ${file}`);
  let record;
  try { record = JSON.parse(readFileSync(file, "utf8")); } catch (error) { fail(`cannot read candidate artifact record: ${error.message}`); }
  if (record.schema !== 1 || record.sourceRevision !== revision || record.platform !== platform || record.architecture !== architecture) {
    fail("candidate compiler artifact identity does not match source, platform, & architecture");
  }
  if (!Array.isArray(record.artifacts) || record.artifacts.length === 0) fail("candidate compiler artifact record is empty");
  for (const artifact of record.artifacts) {
    if (typeof artifact.path !== "string" || !/^[0-9a-f]{64}$/.test(artifact.sha256)) fail("candidate artifact path/hash is invalid");
    const filePath = safeRelative(root, artifact.path, "candidate artifact");
    if (sha256(filePath) !== artifact.sha256) fail(`candidate artifact hash mismatch: ${artifact.path}`);
  }
  if (!record.artifacts.some((artifact) => artifact.kind === "executable" || /\.(?:exe|app)$/i.test(artifact.path))) {
    fail("candidate compiler artifacts contain no executable");
  }
  return { file, record };
}

if (mode === "build") {
  // Right Release binds Windows installer freshness to this call's build start.
  // Keep its build & install in one invocation, then qualify that installed copy.
  // Only ephemeral CI candidates use ad hoc signing; normal dev installs &
  // protected release signing retain their configured Developer ID identity.
  // RightKit rejects this opt-in outside a GitHub-hosted Actions runner.
  const candidateConfig = platform === "macos" ? {
    ...config,
    development: {
      ...config.development,
      targets: {
        ...config.development.targets,
        mac: {
          ...config.development.targets.mac,
          install: { ...config.development.targets.mac.install, signing: "ci-adhoc" },
        },
      },
    },
  } : config;
  runDevelopment({ root: repoRoot, platform: releaseTargetName, config: candidateConfig });
  const input = configuredRecord();
  const entries = cargoCompilerArtifacts(input);
  const cli = entries.find((entry) => entry.target === 'lightcraft-cli' && entry.kind === 'executable');
  if (!cli) fail('candidate must retain exact-revision headless CLI');
  const smoke = spawnSync(cli.path, ['ai', 'compare', '--demo', '--prepare-only', '--out', path.join(root, 'photo-ai-smoke')], { cwd: repoRoot, encoding: 'utf8', windowsHide: true, timeout: 120_000 });
  if (smoke.error || smoke.status !== 0) fail(`headless photo assessment smoke failed: ${smoke.error?.message || smoke.stderr || smoke.status}`);
  const smokeReport = JSON.parse(readFileSync(path.join(root, 'photo-ai-smoke/report.json'), 'utf8'));
  if (smokeReport.mode !== 'prepare-only' || smokeReport.libraryMutated !== false || smokeReport.cases?.length !== 3 || smokeReport.cases.some((item) => item.runs?.length !== 0)) fail('headless photo assessment smoke contract failed');
  runBaseline({ cli: cli.path, root, revision, platform, architecture });
  const materialized = materializeArtifacts(entries, root, {
    sourceRevision: revision,
    platform,
    architecture,
    compilerRecordSha256: sha256(input),
    unsigned: true,
  });
  writeFileSync(path.join(root, "candidate-artifact-summary.json"), `${JSON.stringify(materialized.record, null, 2)}\n`, "utf8");
  const install = config.development.targets[releaseTargetName].install;
  const installed = platform === "macos"
    ? path.join(install.destination, "Contents/MacOS/lightcraft-desktop")
    : install.expectedInstalledPath;
  writeFileSync(path.join(root, "installed-candidate.json"), `${JSON.stringify({
    schema: 1, sourceRevision: revision, platform, architecture,
    path: installed, sha256: sha256(installed),
  }, null, 2)}\n`, "utf8");
  if (platform === "macos") {
    // Retain the SDK-installed bundle's signature & executable modes even if
    // later qualification fails. This is a preview candidate, never a green default.
    const archive = path.join(root, "packages", "Ember-macos-arm64-installed.zip");
    mkdirSync(path.dirname(archive), { recursive: true });
    const packed = spawnSync("/usr/bin/ditto", ["-c", "-k", "--sequesterRsrc", "--keepParent", install.destination, archive], { stdio: "inherit" });
    if (packed.error || packed.status !== 0) fail(`installed Mac bundle archive failed: ${packed.error?.message || packed.status}`);
    writeFileSync(path.join(root, "installed-bundle.json"), `${JSON.stringify({
      schema: 1, sourceRevision: revision, platform, architecture,
      path: path.relative(root, archive), sha256: sha256(archive),
      installedExecutableSha256: sha256(installed), nativeQualification: "pending",
    }, null, 2)}\n`, "utf8");
  }
  console.log(`desktop candidate build: PASS ${platform}/${architecture} (${materialized.record.artifacts.length} Cargo artifacts)`);
} else {
  const { record } = candidateRecord();
  const qualification = await import("./candidate-qualification.mjs");
  await qualification.qualify(record, root);
  writeFileSync(path.join(root, "candidate-artifact-summary.json"), `${JSON.stringify(record, null, 2)}\n`, "utf8");
  console.log(`desktop candidate check: PASS ${platform}/${architecture} (${record.artifacts.length} artifacts)`);
}
