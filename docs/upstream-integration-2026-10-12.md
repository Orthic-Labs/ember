# Upstream integration — 2026-10-12

Source candidate incorporates `storytold/lightcraft` through
`25ef65c81eef81833a50ff5b61f8676d80611b06` (workspace version 0.7.0),
382 commits after previously fetched `0ba118de`. Ember base is
`bccf37134a2970550e71ee4e2fbf7fdad267a341`.

Imported source includes expanded RAW decoding, camera calibration, process versions,
display profiles, denoise infrastructure, face detection/recognition, lens profiles,
folder synchronization, background activities & UI fixes. Source presence does not
establish output quality, model availability or native qualification.

Integration retains Ember's React/Tauri host, RightKit pins, local culling & Personal
Auto source, bounded preview shutdown, hostile-input guards, import policy options,
archive validation & recoverable GPU startup. Manual conflict review reconciled new
render metadata, denoise source fields, import cancellation & window renderer fallback.

Product UI & locale copy use Ember/Orthic Labs; catalog import labels use LRCAT.
Interoperability command IDs, file schemas, legacy migration paths, Adobe RGB & legal
attribution remain stable. Maintainer-authorized read-only internal research is allowed
outside this repository; personal libraries remain unchanged.

Public fork builds use generated RightKit workflows. Retained upstream build workflows
are restricted to upstream repository identity. Local checks are source/static-only.
Remote CI must qualify this exact source before promoting any desktop artifact.

Static checks completed during preparation: desktop contracts, Rust formatting/parser
acceptance & locale JSON validity. Remote compile, tests, parity, assets & target-native
qualification are tracked in the integration pull request.
