import { execFileSync } from "node:child_process";
import { fail, repoRoot, sourceIdentity, writeOutput } from "./lib.mjs";

const version = process.env.RIGHT_GIT_RELEASE_VERSION;
if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version ?? "")) fail("RIGHT_GIT_RELEASE_VERSION must be stable SemVer");
const revision = sourceIdentity();
let head;
try {
  head = execFileSync("git", ["rev-parse", "HEAD"], { cwd: repoRoot, encoding: "utf8" }).trim();
} catch (error) {
  fail(`cannot resolve checked-out source revision: ${error.message}`);
}
if (head !== revision) fail(`checked-out source ${head} does not match admitted ${revision}`);

const signed = process.env.RIGHT_GIT_SIGNED_QUALIFICATION === "true";
const publish = process.env.RIGHT_GIT_PUBLISH === "true";
const dryRun = process.env.RIGHT_GIT_DRY_RUN === "true";
// signing runs in the generated windows-sign/macos-sign jobs; publication stays with right-release
if (publish) fail("this candidate lane does not publish; publish an exact build through right-release");
if (publish && !signed) fail("publication requires signed qualification");
if (publish && dryRun) fail("dry-run cannot publish");
if (process.env.RIGHT_GIT_WORKFLOW_REF && !dryRun && process.env.RIGHT_GIT_WORKFLOW_REF !== "refs/heads/main") fail("release candidate must run from main");
if (process.env.RIGHT_GIT_RUN_ATTEMPT && process.env.RIGHT_GIT_RUN_ATTEMPT !== "1" && !dryRun) fail("release candidate cannot reuse a workflow run");

writeOutput({
  version,
  source_revision: revision,
  signed_qualification: signed,
  publish,
  dry_run: dryRun,
  artifact_suffix: `${version}-${revision}`,
});
console.log(`desktop release admission: PASS ${version} ${revision}`);
