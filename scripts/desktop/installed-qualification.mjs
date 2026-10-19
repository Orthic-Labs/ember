import { existsSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { runCargoSync } from "@rightkit/release/managed-cargo.mjs";
import { currentPlatform, fail, finalizedRoot, repoRoot, safeRelative, sha256, sourceIdentity } from "./lib.mjs";

if (process.env.GITHUB_ACTIONS !== "true") fail("installed qualification is GitHub Actions-only");
const platform = currentPlatform();
const root = path.resolve(finalizedRoot());
const revision = sourceIdentity();
const architecture = process.env.RIGHT_GIT_RELEASE_ARCHITECTURE || process.env.RIGHT_GIT_TARGET_ARCH;
if (!architecture) fail("installed qualification requires RIGHT_GIT_RELEASE_ARCHITECTURE from generated target matrix");
const configured = process.env.RIGHT_GIT_COMPILER_ARTIFACT_RECORD;
const recordPath = configured
  ? (path.isAbsolute(configured) ? configured : path.resolve(repoRoot, configured))
  : path.join(root, "compiler-artifacts.json");
if (!existsSync(recordPath)) fail(`installed compiler artifact record is missing: ${recordPath}`);
let record;
try { record = JSON.parse(readFileSync(recordPath, "utf8")); } catch (error) { fail(`cannot read installed compiler artifact record: ${error.message}`); }
if (record.schema !== 1 || record.sourceRevision !== revision || record.platform !== platform || record.architecture !== architecture) {
  fail("installed artifact identity does not match source, platform, & architecture");
}
if (!Array.isArray(record.artifacts) || record.artifacts.length === 0) fail("installed compiler artifact record is empty");
const executable = record.artifacts.find((artifact) => artifact.kind === "executable" || /\.(?:exe|app)$/i.test(artifact.path));
if (!executable) fail("installed artifact record contains no executable");
const executablePath = safeRelative(root, executable.path, "installed executable");
for (const artifact of record.artifacts) {
  const file = safeRelative(root, artifact.path, "installed artifact");
  if (sha256(file) !== artifact.sha256) fail(`installed artifact hash mismatch: ${artifact.path}`);
}
const evidenceRoot = path.resolve(process.env.RIGHT_GIT_QUALIFICATION_EVIDENCE_ROOT || path.join(root, "qa-evidence"));
const env = {
  ...process.env,
  RIGHTKIT_QA_HIDDEN: "1",
  RIGHTKIT_QA_UI_BINARY: executablePath,
  RIGHTKIT_QA_EVIDENCE: evidenceRoot,
  RIGHTKIT_QA_ARTIFACT_ROOT: root,
  RIGHTKIT_QA_SOURCE_REVISION: revision,
  RIGHTKIT_QA_ARCHITECTURE: architecture,
  RIGHTKIT_QA_WEBVIEW: platform === "macos" ? "WKWebView" : "WebView2",
  RIGHTKIT_QA_SCENARIOS: path.resolve(repoRoot, "fixtures/desktop/native-journey.json"),
  RIGHTKIT_QA_CATALOG: path.resolve(repoRoot, "fixtures/desktop/catalog.json"),
  RIGHTKIT_QA_BASELINE: path.resolve(repoRoot, "fixtures/desktop/installed-baseline.json"),
  RIGHTKIT_QA_INSTALLED_ARTIFACT_SHA256: executable.sha256,
  // rightkit-qa 0.7 refuses an app in an installed-apps location unless this stage is named.
  RIGHT_RELEASE_QA_STAGE: "installed",
};
const args = ["test", "--locked", "-p", "lightcraft-desktop", "--test", "native_qualification", "--features", "qa-native", "--", "--ignored", "--nocapture"];
console.log(`desktop installed qualification: rightkit Cargo ${args.join(" ")}`);
const result = runCargoSync(args, { cwd: repoRoot, env, stdio: "inherit", shell: process.platform === "win32" });
if (result.error) fail(`installed qualification failed to start: ${result.error.message}`);
if (result.status !== 0) fail(`installed qualification exited ${result.status}`);
function files(dir) {
  if (!existsSync(dir)) return [];
  const found = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const file = path.join(dir, entry.name);
    if (entry.isDirectory()) found.push(...files(file));
    else if (entry.isFile()) found.push(file);
  }
  return found;
}
const evidence = files(evidenceRoot);
if (!evidence.some((file) => path.basename(file) === "summary.jsonl") || !evidence.some((file) => path.basename(file) === "evidence.json")) {
  fail("RightKit QA did not produce summary.jsonl & evidence.json");
}
console.log(`desktop installed qualification: PASS ${platform}/${architecture} via rightkit-qa control`);
