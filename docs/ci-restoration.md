# CI restoration record

## Restoration confirmed, 2026-10-10

Maintainer selected GitHub's fork `re-enable` control. Dispatching existing
`Packaging lint` workflow on main succeeded & created
[run 38053737558](https://github.com/Orthic-Labs/lightcraft/actions/runs/38053737558)
for source revision `561a076eed0d5bb506de3cdf09048ccd435ad8e2`.
This confirms workflow execution is available again; validation results still
require completed runs bound to their exact source revision.

Full workspace CI runs on the next main push. Unsigned candidate qualification
then uses that exact revision on macOS & Windows, with signing/publication
disabled. Published RightKit foreground readers still require repair before
macOS background evidence can qualify. Repair proposal is isolated outside
shared SDK checkout; no SDK changes or dependency upgrades have been applied.

## RightKit ownership handoff, 2026-10-10

Adrian assigned SDK work to Claude chat **Rightkit Mac new** & instructed Ember
chat to leave RightKit work there. Repair request, isolated reviewed patch,
native failure evidence & required qualification were sent through Pulse to
`Rightkit Mac new on Adrian’s Mac Mini` (message
`56d6f028-baa9-4e64-bbab-ff8ba4403484`). Corrected full proposal base SHA was
sent separately. Pulse reported `sent`
without recipient verdict; acknowledgment is pending. Do not resend or poll
its inbox. No SDK files, versions or packages were changed by Ember chat.

SDK owner will handle repair, native feature-matrix qualification, managed
publication & dependency integration. Ember retains existing pins & reruns
exact-revision hidden native QA with existing hard foreground guard intact. [#211](https://github.com/Orthic-Labs/ember/issues/211)

## Restored native evidence, 2026-10-10

[Candidate run 38055623316](https://github.com/Orthic-Labs/lightcraft/actions/runs/38055623316)
built source `93678dd1b9977d6ee509679be164fe2ec3b44113` successfully on
macOS/arm64 & Windows/x86_64. Both builds validated & retained procedural
CPU denoise receipts before hidden native QA. Extracted evidence matches
stage-summary SHA-256 entries: 48 Mac files & 43 Windows files.

Windows passed 14 of 15 hidden journeys. Culling Apply failed synchronously;
dialog showed a generic stale-photo message, so exact validation rejection
remains unresolved. [#209](https://github.com/Orthic-Labs/ember/issues/209) Mac repeated unavailable foreground PID/name evidence
across all journeys; 12 journey bodies passed, while culling Apply, inspector
collapse & virtual-grid geometry failed. Native qualification remains failed.

Mac same-parent probes held viewport, parent & sibling slot constant. Fresh
stage layout restored collapsed columns from `690px 8px 336px` to
`990px 0px 44px`; fresh grid spacer restored computed height from `24354px`
to its inline `79810px`. Both results persisted at 64 ms. Direct inline style
changes & viewport bounce did not restore geometry. Ember follow-up preserves
stateful workspaces while testing isolated DOM replacement; SDK foreground
repair remains with its assigned owner. [#210](https://github.com/Orthic-Labs/ember/issues/210)

## Follow-up evidence, 2026-10-10

[Candidate run 38058116433](https://github.com/Orthic-Labs/lightcraft/actions/runs/38058116433)
built source `d89a0ff1fff11bd0166bb2327dec088e35e0983c` on both targets.
Extracted evidence matches summary SHA-256 entries: 63 Mac files & 60 Windows
files. Windows again passed 14/15 journey bodies; Mac passed 12/15 with the
same unavailable foreground evidence. Neither target qualified.

The culling dialog now exposes its exact error: `cull proposal must be an object`.
Source fix `237a58bf` corrects Apply parsing that selected the numeric `version`
field instead of its proposal object; a real JSON round-trip regression covers
direct & nested proposal shapes. Native Apply/undo must still verify that fix.

Mac grid spacer replacement restored computed height, but its window retained
computed `top: 0` despite inline `top: 78618.4px`. Replacing that window alone
restored computed top. A zero-size child invalidation probe did not restore
stage columns. Follow-up source refreshes geometry shells while retaining
photo, StageWorkspace & Inspector content through a stable portal host; native
regressions require preserved focus, preview identity, decoded pixels & correct
layout. SDK foreground repair remains with **Rightkit Mac new**.

Full workspace run
[38059311663](https://github.com/Orthic-Labs/lightcraft/actions/runs/38059311663)
at `ea470b4f7f68235e0e59ba3bbfa4670b5ce57803` reached clippy after earlier
test compilation fixes. Four iterator-access lints & one test-module ordering
lint stopped later gates. Follow-up source resolves all five without suppressions;
tests & exact-artifact qualification await generated runs.

## Retained-layout native evidence, 2026-10-10

[Candidate run 38060467035](https://github.com/Orthic-Labs/lightcraft/actions/runs/38060467035)
built `e51ce9fcfc53102e8a9e0f8a29860420c027eced` on both targets. Extracted
evidence matches summary SHA-256 entries: 64 Mac files & 65 Windows files.
Both targets passed 12/15 journey bodies; culling Apply, Inspector identity
predicate & scalability failed. Windows foreground guards passed; Mac guards
still lacked reliable PID/name evidence. Neither target qualified.

Mac collapsed layout now measures `990px 0px 44px`, retaining StageWorkspace
& Inspector identity with painted preview & full filmstrip. Inspector predicate
also demanded identity of a deliberately keyed preview image, which can change
during proxy promotion; follow-up instead requires stable preview frame,
decoded current pixels, retained controls/focus & correct geometry. Scalability
reached its new focus/preview retention check, then failed because Display had
no Escape dismissal, leaving the next toggle closed. Follow-up implements real
Escape dismissal.

Culling now reaches fresh measurement validation but rejects equivalent
wire-level f64 numbers. Follow-up re-emits validated f32 rows before comparing
fresh results, preserving exact binding, source, flags, revision & action checks;
an adjacent-equivalent-f64 regression exercises completed Apply. Group presence
now hashes separately, with a regression rejecting absent/zero-group tampering.
These source repairs require generated tests & another exact-revision native run. RightKit
foreground repair remains with its assigned owner.

## Workspace tests reached, 2026-10-10

[CI run 38062024596](https://github.com/Orthic-Labs/lightcraft/actions/runs/38062024596)
at `143ecbd6be6ddde253affb2e5f341bed7fed3e1b` passed frontend checks/build,
Rust formatting & workspace/all-target clippy. Two CLI scorer tests failed
because their fixtures omitted mandatory explicit split metadata. Follow-up
corrects those fixtures while preserving split validation & malformed-winner
rejection. CI now uses `cargo test --workspace --no-fail-fast` to collect
failures across test binaries; any failed binary still fails the gate.

[Native run 38062743357](https://github.com/Orthic-Labs/lightcraft/actions/runs/38062743357)
uses explicit source `0877ab27d275980269889963534f5af614be1043`, with signing
& publication disabled. Candidate branch keeps that source available without
cancelling the earlier main CI run. Both builds stopped while compiling the
new diagnostic helper because its loop assigned an immutable binding; no
journeys ran. Source `f4b19b5e` corrects that binding.

## Culling & preview follow-up, 2026-10-10

[Native run 38064000491](https://github.com/Orthic-Labs/lightcraft/actions/runs/38064000491)
built `f4b19b5e0b1c2fd93de97f08111798afd33b7958` on both targets. Extracted
evidence matches summary SHA-256 entries: 55 Mac files & 62 Windows files.
Each target passed 14/15 journey bodies. Culling Apply/undo passed on both.
Windows foreground guards passed; Mac guards still lacked reliable PID/name
evidence, so its official summary remains failed for all 15 journeys.

Mac Inspector collapse retained StageWorkspace, Inspector & decoded preview
frame with correct `990px 0px 44px` columns, but lost focus. Source `4f2ea8dd`
restores captured focus after React's keyed shell commit, clears stale captures
& refuses targets outside the retained host. Windows scalability retained cell
focus but failed an image identity predicate during thumbnail proxy promotion;
its follow-up must retain cell/frame identity while validating decoded current
pixels. Focus & geometry assertions remain mandatory.

Same-photo editing stress recorded zero observed blank previews, Retry banners,
image decode errors or blank filmstrip samples: 40 timer samples on Mac & 755
samples on Windows. Mac's hidden WebKit reported zero animation frames; these
receipts do not measure smooth frame delivery or photographic accuracy.

[Unsigned follow-up 38065705019](https://github.com/Orthic-Labs/lightcraft/actions/runs/38065705019)
uses explicit source `4f2ea8dd5bc2e16031886a6f13281cecc7689d53`, with signing
& publication disabled. SDK repair & dependency integration remain assigned
to **Rightkit Mac new**; Ember retains published pins & hard foreground checks.

## Full workspace failure collection, 2026-10-10

[CI run 38064075030](https://github.com/Orthic-Labs/lightcraft/actions/runs/38064075030)
at `f4b19b5e0b1c2fd93de97f08111798afd33b7958` passed frontend checks/build,
Rust formatting & all-target clippy. `--no-fail-fast` collected 11 test failures:
two desktop-host task completion waits, four culling validation fixtures,
one Windows path expectation, three pipeline measurement/spatial fixtures &
one loupe draft sizing expectation. Parity, layers, assets & WASM did not run
after the failed test gate.

Follow-up retains production validation & corrects fixtures against actual
contracts: group presence participates in test-side binding hashes; coherent
aggregate counts let tampering reach binding validation; normalized folder
paths preserve their supplied separator; tiny nonzero signatures have unit
self-similarity; spatial chroma uses the existing colour conversion & WB
overflow exercises the blue coefficient. Task waits use bounded monotonic
deadlines with progress diagnostics. Loupe draft checks use physical canvas
size plus the existing rounding allowance while still prohibiting full-size
drafts. Generated CI must verify these changes.

## Suspension history

Checked 2026-10-10 against `Orthic-Labs/lightcraft` at main revision
`2fef37a4c466acd8c0ae88a1ac5a1d0e1218dbc0`.

Rechecked after source snapshot `3036f57cd1ee7bae8fd0183d29be34ced46335a9` was pushed to fork main: exact-revision run listing is empty & refreshed Actions page still displays same fork-usage suspension banner. Hidden browser is signed out; no maintainer re-enable control is available in current session. No restoration calls were repeated during this check.

Rechecked source snapshot `26100c66092503d4dca440e99f2903f863304617`: exact-revision Actions API returns zero runs. Available in-app browser remains signed out & public Actions page still shows fork-usage suspension. No alternate connected authenticated browser is available; no settings reset or dispatch was repeated.

GitHub reports repository Actions enabled, all actions allowed, workflow
permissions `read`, and both `lightcraft-desktop-workspace-ci` and
`release-candidate` active. Repository metadata also reports `disabled:false`,
with current token permissions including `admin:true`. Supported restoration
calls were made once:

- `gh workflow enable ci.yml` → exit 0
- `gh workflow enable release-candidate.yml` → exit 0
- `PUT /repos/Orthic-Labs/lightcraft/actions/permissions` with
  `{"enabled":true,"allowed_actions":"all"}` → exit 0
- `PUT /repos/Orthic-Labs/lightcraft/actions/permissions/workflow` with
  `{"default_workflow_permissions":"read","can_approve_pull_request_reviews":false}` → exit 0

Dispatching generated `release-candidate.yml` for the current revision then
failed before a run was created:

```
HTTP 422: Actions has been disabled for this repository.
POST /repos/Orthic-Labs/lightcraft/actions/workflows/377917643/dispatches
```

The same repository state returns HTTP 409 for selected-actions settings
(`All actions and workflows are allowed on this repository`). Organization
Actions policy reads return HTTP 403: `You must be an org admin or have the
actions policies fine-grained permission` and require `admin:org`; this is an
access boundary, not proof that organization policy caused the failure.

Comparison with public Orthic repositories narrows scope. `pulse`, `legion`,
and `sellright` each report `disabled:false`, repository Actions `enabled:true`,
`allowed_actions:all`, active workflows, and recent runs (including
`pulse` run `38036036549`, `legion` run `38034999218`, and `sellright` run
`38030506274`). LightCraft alone returns the dispatch HTTP 422, so confirmed
cause is fork-usage suspension at GitHub Actions service layer. Public Actions
page [`Orthic-Labs/lightcraft/actions`](https://github.com/Orthic-Labs/lightcraft/actions)
shows: `Workflows are not currently being run on this fork due to the scale of
GitHub Actions usage. A repository maintainer can choose to re-enable these.`
This establishes fork suspension; it does not establish organization policy.

Last usable evidence predates current main: CI run `37870029202` passed at
`09fdad5e44b92dee301a272a8a9b31faf1ae15c4`; candidate run `37871082119` ran
both targets at that revision, with Windows passing and macOS hidden native QA
failing. No current-revision run exists because GitHub rejects dispatch before
workflow execution.

Mac candidate job `113631654261` failed its hard foreground guard: sampled names were `LSDisplayName`,
`owned_frontmost_pids` was empty & PID sampling was unavailable. This is missing proof of hidden operation, not proof
that an owned process activated. Native IPC/scalability journeys also reported stale hidden WebKit layout. Current
source retains foreground assertions & does not launch/install another app to work around these failures.

Registry-source audit of `rightkit-qa` 0.2.12 versus 0.2.14 found identical control/process/guard implementations.
0.2.14 changes ffprobe precedence, hashing provider & dependency edges; it does not repair unavailable frontmost PID
sampling. Exact 0.2.12 pin is retained. Qualification requires reliable PID evidence & passing layout journeys, not
relaxing `owned_never_frontmost` when observed owned PID list is empty.

`rightkit-control` 0.1.9/0.1.10/0.1.11 have identical `mac.rs` frontmost readers. Separate `lsappinfo` calls parse an
exact `pid =` substring for PID & the first quoted token for name; missing PID/inventory evidence fails closed. Name
`LSDisplayName` is therefore not reliable application identity. QA StopReport does not distinguish unavailable PID,
inventory failure & missing identity registration. Required SDK repair is robust PID acquisition plus explicit failure
diagnostics, then published dependency update & exact-artifact native qualification. No published upgrade solves it.

Workflow regeneration was inspected before retaining this correction. Current
`@rightkit/release` ownership source defines required hosts as `windows` and
`macos`; its trust-path selector throws `ownership approval unsupported
platform: linux`. Prior candidate admission on `macos-15` completed ownership
checks, while no Linux admission evidence exists. Template conformity therefore
cannot replace this runner until published RightKit source supports Linux.

GitHub’s [repository Actions settings guidance](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/enabling-features-for-your-repository/managing-github-actions-settings-for-a-repository)
states that this banner indicates a separate GitHub-controlled disabled state;
changing repository settings does not restore access. One bounded reset attempt
preserved `allowed_actions:all`: `PUT .../actions/permissions` with
`{"enabled":false}` succeeded, then `{"enabled":true,"allowed_actions":"all"}`
succeeded. A first invalid false request including `allowed_actions` returned
HTTP 409 because that field is accepted only when enabled. One post-reset
dispatch still returned HTTP 422 `Actions has been disabled for this
repository`; no run was created.

Smallest remaining action: repository maintainer select public Actions page’s
supported `re-enable` control. GitHub documents this separate disabled state as
requiring Support review if settings changes do not restore access; repository
API settings cannot clear fork-usage suspension. Require completed
generated-workflow run for exact current SHA as CI evidence.

`pnpm exec right-git drift .` initially reported `release-candidate.yml`
mismatch. `pnpm exec right-git sync .` regenerated it, changing only admission
runner `macos-15` to template runner `ubuntu-24.04`. The supported macOS
correction is retained; resulting drift is intentional until template or SDK
adds Linux ownership approval.
