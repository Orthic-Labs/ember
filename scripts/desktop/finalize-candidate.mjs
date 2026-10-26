// Stages and packages the exact unsigned release candidate for RightKit's
// signed targets (`right-release build --platform mac|win`). It holds no
// signing, notarization, or installer logic: `@rightkit/release` signs the
// staged executables named by `sign.prePackageFiles` and notarizes the archive
// this script packages. This script only binds those inputs to the candidate.
//   node scripts/desktop/finalize-candidate.mjs stage    (prePackage)
//   node scripts/desktop/finalize-candidate.mjs package  (package)
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import path from "node:path";
import { fail, loadReleaseConfig, requireEnv, repoRoot, safeRelative, sha256, sourceIdentity } from "./lib.mjs";
import { requireDoor } from "@rightkit/release/door.mjs";
// Runs only as a step right-release runs inside a rightkit-gate --to stage (class A first); by hand it is refused.
requireDoor("scripts/desktop/finalize-candidate.mjs");

const mode = process.argv[2];
if (!new Set(["stage", "package"]).has(mode)) fail("usage: node scripts/desktop/finalize-candidate.mjs <stage|package>");
if (process.env.GITHUB_ACTIONS !== "true") fail("candidate finalization is GitHub Actions-only");

const platform = requireEnv("RIGHT_GIT_RELEASE_PLATFORM");
const architecture = requireEnv("RIGHT_GIT_RELEASE_ARCHITECTURE");
const revision = sourceIdentity();
const layouts = {
  macos: { arch: "arm64", exe: "", out: "mac", zip: "ditto" },
  windows: { arch: "x86_64", exe: ".exe", out: "windows", zip: "tar" },
};
const layout = layouts[platform];
if (!layout || layout.arch !== architecture) fail(`unsupported release platform/architecture: ${platform}/${architecture}`);

const { config } = await loadReleaseConfig();
const version = config.version;
const stage = path.join(repoRoot, "dist", "native", `${platform}-${architecture}`, `ember-${version}`);
const output = path.join(repoRoot, "dist", "releases", layout.out, version, architecture);
const archive = path.join(output, `ember-${version}-${platform}-${architecture}.zip`);
const required = ["lightcraft-desktop", "lightcraft-cli", "ember-av1-worker"];

function readRecord(root) {
  const file = path.join(root, "compiler-artifacts.json");
  if (!existsSync(file)) fail(`compiler-artifacts.json is missing: ${file}`);
  let record;
  try { record = JSON.parse(readFileSync(file, "utf8")); } catch (error) { fail(`cannot read compiler artifact record: ${error.message}`); }
  if (record.schema !== 1 || record.sourceRevision !== revision || record.platform !== platform || record.architecture !== architecture) {
    fail("compiler artifact identity does not match admitted source, platform, & architecture");
  }
  if (!Array.isArray(record.artifacts)) fail("compiler artifact record has no artifacts");
  return record;
}

function executables(record, root) {
  const found = new Map();
  for (const artifact of record.artifacts) {
    if (typeof artifact.path !== "string" || !/^[0-9a-f]{64}$/.test(artifact.sha256 ?? "")) fail("artifact path/hash is invalid");
    if (artifact.kind !== "executable" || !required.includes(artifact.target)) continue;
    if (found.has(artifact.target)) fail(`duplicate executable for ${artifact.target}`);
    const file = safeRelative(root, artifact.path, "candidate artifact");
    if (sha256(file) !== artifact.sha256) fail(`candidate artifact hash mismatch: ${artifact.path}`);
    found.set(artifact.target, { file, record: artifact });
  }
  for (const name of required) if (!found.has(name)) fail(`candidate lacks executable ${name}`);
  return found;
}

if (mode === "stage") {
  const candidate = path.resolve(requireEnv("LEGION_UNSIGNED_CANDIDATE_ROOT"));
  const found = executables(readRecord(candidate), candidate);
  rmSync(stage, { recursive: true, force: true });
  mkdirSync(path.join(stage, "bin"), { recursive: true });
  for (const [name, { file }] of found) {
    const target = path.join(stage, "bin", `${name}${layout.exe}`);
    copyFileSync(file, target);
    chmodSync(target, 0o755);
  }
  console.log(`desktop finalize: staged ${found.size} exact candidate executables for ${platform}/${architecture}`);
} else {
  const staged = required.map((name) => path.join(stage, "bin", `${name}${layout.exe}`));
  for (const file of staged) if (!existsSync(file)) fail(`staged executable is missing: ${file}`);
  rmSync(output, { recursive: true, force: true });
  mkdirSync(output, { recursive: true });
  // Post-signing hashes, in the same record shape later stages read.
  const record = {
    schema: 1,
    sourceRevision: revision,
    platform,
    architecture,
    version,
    signed: true,
    artifacts: required.map((name, index) => ({
      path: `bin/${name}${layout.exe}`,
      sha256: sha256(staged[index]),
      kind: "executable",
      target: name,
    })),
  };
  writeFileSync(path.join(stage, "compiler-artifacts.json"), `${JSON.stringify(record, null, 2)}\n`, "utf8");
  copyFileSync(path.join(stage, "compiler-artifacts.json"), path.join(output, "compiler-artifacts.json"));
  const packed = layout.zip === "ditto"
    ? spawnSync("/usr/bin/ditto", ["-c", "-k", "--sequesterRsrc", stage, archive], { stdio: "inherit" })
    : spawnSync(path.join(process.env.SystemRoot || "C:\\Windows", "System32", "tar.exe"), ["-a", "-c", "-f", archive, "-C", stage, "."], { stdio: "inherit", windowsHide: true });
  if (packed.error || packed.status !== 0) fail(`archive failed: ${packed.error?.message || packed.status}`);
  console.log(`desktop finalize: packaged ${path.relative(repoRoot, archive)}`);
}
