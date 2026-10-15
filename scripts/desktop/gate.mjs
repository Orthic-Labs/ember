import { appendFileSync, existsSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import path from "node:path";
import { runCargoSync } from "@rightkit/release/managed-cargo.mjs";
import { repoRoot, fail, sha256, writeOutput } from "./lib.mjs";

function run(command, args, label, env = process.env) {
  console.log(`desktop gate: ${label}`);
  const result = command === "cargo"
    ? runCargoSync(args, { cwd: repoRoot, env, stdio: "inherit", shell: process.platform === "win32" })
    : BunlessSpawn(command, args, env);
  if (result.error) fail(`${label}: ${result.error.message}`);
  if (result.status !== 0) fail(`${label}: exited ${result.status}`);
}

function BunlessSpawn(command, args, env) {
  // Keep package-manager invocations explicit while Cargo goes through RightKit's
  // managed-cargo adapter. This avoids requiring a `rightkit` executable on GHA.
  return spawnSync(command, args, { cwd: repoRoot, env, stdio: "inherit", shell: process.platform === "win32" });
}

function lockfileReady(lockfile) {
  if (!existsSync(lockfile)) return false;
  const text = readFileSync(lockfile, "utf8");
  return text.includes('name = "rightkit-qa-driver"') && text.includes('name = "rightkit-qa"');
}

function bootstrapLockfile(lockfile) {
  if (lockfileReady(lockfile)) {
    const probe = runCargoSync(["metadata", "--locked", "--format-version", "1"], { cwd: repoRoot, encoding: "utf8", maxBuffer: 16 * 1024 * 1024, windowsHide: true });
    if (!probe.error && probe.status === 0) return;
    const diagnostic = probe.error?.message || probe.stderr || `Cargo metadata exited ${probe.status}`;
    if (!/cannot update the lock file|lock file .* needs to be updated/i.test(diagnostic)) fail(`Cargo lock validation: ${diagnostic}`);
    console.log("desktop gate: dependency changes require verified Cargo.lock refresh");
  }
  run("cargo", ["generate-lockfile"], "Cargo.lock bootstrap via @rightkit/release");
  if (!lockfileReady(lockfile)) fail("Cargo.lock bootstrap did not resolve pinned rightkit-qa-driver & rightkit-qa");
  const bytes = readFileSync(lockfile);
  const encoded = bytes.toString("base64");
  const digest = sha256(lockfile);
  if (encoded.length > 1000000) fail(`generated Cargo.lock exceeds bounded transfer size (${encoded.length} base64 bytes)`);
  writeOutput({ cargo_lock_sha256: digest, cargo_lock_bytes_base64: encoded });
  console.log(`LC_CARGO_LOCK_SHA256=${digest}`);
  console.log("LC_CARGO_LOCK_BASE64_BEGIN");
  for (let offset = 0; offset < encoded.length; offset += 120) console.log(encoded.slice(offset, offset + 120));
  console.log("LC_CARGO_LOCK_BASE64_END");
  const summary = process.env.GITHUB_STEP_SUMMARY;
  if (summary) appendFileSync(summary, `\n### Cargo.lock bootstrap required\n\nsha256: ${digest}\nbytes: ${bytes.length}\n\nRetrieve cargo_lock_bytes_base64 from job output, decode to Cargo.lock, commit it, then rerun gate.\n`, "utf8");
  fail(`Cargo.lock bootstrap complete (${digest}); commit generated lock and rerun gate with --locked`);
}

if (process.env.GITHUB_ACTIONS !== "true") fail("desktop CI gate is GitHub Actions-only; use node scripts/desktop/static-check.mjs locally");
if (process.env.RIGHT_GIT_RUST_REQUIRED !== "true") fail("right-git manifest must set rustCache.requiredForGate=true for desktop gate");

run(process.execPath, ["scripts/desktop/static-check.mjs"], "static contracts");
run("pnpm", ["run", "typecheck"], "React typecheck");
run(process.execPath, ["--test", "scripts/desktop/mask-geometry.test.mjs"], "mask geometry regression tests");
run(process.execPath, ["--test", "scripts/desktop/slider-command-queue.test.mjs"], "slider command queue regression tests");
run("pnpm", ["run", "build"], "React build");
const lockfile = path.join(repoRoot, "Cargo.lock");
bootstrapLockfile(lockfile);
run("cargo", ["run", "--locked", "-p", "xtask", "--", "ci"], "Rust fmt, clippy, test, parity, layers, assets & WASM");
run("cargo", ["run", "--locked", "-p", "lightcraft-engine", "--example", "import_bench"], "1,300-file procedural import benchmark", {
  ...process.env, COUNT: "1300", JPEG_EDGE: "512", RAW_EDGE: "512",
});
console.log("desktop CI gate: PASS");
