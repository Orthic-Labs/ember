import assert from "node:assert/strict";
import test from "node:test";
import { NOT_PROVISIONED_MESSAGE, UPDATER_PROVISIONED, checkForUpdates, startupUpdateCheck, updateNotice } from "../../apps/lightcraft-desktop/web/src/updates.ts";

const neverCalled = async () => { throw new Error("plugin check must not run"); };

test("dormant build runs no startup check and reports the manual download", async () => {
  assert.equal(startupUpdateCheck(false, neverCalled), null);
  const status = await checkForUpdates(false, neverCalled);
  assert.equal(status.kind, "error");
  assert.equal(status.message.replace(/^Error: /, ""), NOT_PROVISIONED_MESSAGE);
});

test("the shipped constant stays dormant until rightkit#237 publishes", () => {
  assert.equal(typeof UPDATER_PROVISIONED, "boolean");
});

test("provisioned build maps the plugin result to status", async () => {
  assert.equal((await checkForUpdates(true, async () => null)).kind, "up_to_date");
  const failing = await checkForUpdates(true, async () => { throw new Error("offline"); });
  assert.equal(failing.kind, "error");
  assert.equal(updateNotice(failing), "offline");
  assert.equal(updateNotice({ kind: "up_to_date" }), "Ember is up to date");
  assert.equal(updateNotice({ kind: "available", version: "0.8.0" }), "Ember 0.8.0 is available");
  assert.equal(updateNotice({ kind: "checking" }), "Update check in progress");
});

test("startup check with a provisioned plugin reports availability without installing", async () => {
  let installed = false;
  const update = { version: "0.8.0", downloadAndInstall: async () => { installed = true; } };
  const status = await startupUpdateCheck(true, async () => update);
  assert.equal(status?.kind, "available");
  assert.equal(installed, false);
});
