# Photo assessment experiment

Read-only, provider-neutral contract & native OpenRouter transport. No catalog/edit/flag API is available to this crate. All hosts expose `ai.models`, `ai.schema` & `ai.validate`; native CLI adds `ai compare` with rendered references & receipts. Current `develop.auto` stays deterministic.

## Comparison

Use a CI-qualified `lightcraft-cli` binary. Configure `OPENROUTER_API_KEY` in its process environment securely (never in committed files or shell history), then:

```text
lightcraft-cli ai compare --demo --out /tmp/photo-ai-run --repeat 2 --budget-usd 0.50
lightcraft-cli ai compare /path/to/photo.jpg --out /tmp/photo-ai-one --model deepseek/deepseek-v4.1-flash --repeat 3
lightcraft-cli ai compare --demo --out /tmp/photo-ai-reference --prepare-only
```

Explicit files are opened in a fresh ephemeral session; saved libraries/folders are unsupported. Only metadata-free 512px PNG proxies leave process. Tone controls are reset to zero for input; WB/profile & other existing settings are preserved. Source bytes, catalog state, edits & ratings are never changed. CLI exits nonzero on interrupted/budget-stopped runs & checkpoints completed receipts before rendering their suggestions. Default admission budget is $0.50, sufficient for the three demo frames, five models & two repeats at frozen reserve rates.

Five-model shortlist is frozen against 9 October 2026 public capability/model-version metadata: DeepSeek V4.1 Flash, Gemini3.8 Flash, Gemini3.5 Flash-Lite, Qwen3.8 Flash & Haiku5.5. Metadata preflight requires image/structured-output support & matching canonical revision; endpoint routing requires strict schema parameters, no fallback, denied data collection & zero data retention. Lack of an eligible endpoint is a recorded failure, not permission to relax privacy or schema enforcement. API response records actual served model/provider, latency, usage/cost, errors & exact uploaded-byte hash. Canonical API metadata is evidence, not proof of an undisclosed provider weight revision.

No retries, tools, file uploads, streaming or durable response cache. At most twelve files, five models & three repeats. Requests cap completion tokens at2048, bodies at4MiB, responses at256KiB, & deadline at45s. OS DNS resolution can outlive request deadline; subsequent I/O checks it. Interrupted billing is unknown & stops further calls. Budget is an estimated admission reserve (20K input tokens + capped output at frozen upper rates); use provider-key spending limits for hard enforcement. Missing usage cost stops comparison. Provider price limits cap rates; billed totals are recorded without pretending to predict image-token billing exactly.

Model JSON must contain all fields & eight exact, finite, bounded controls for `adjust` with confidence≥0.7. Invalid JSON, unsupported controls, duplicate recipe fields, partial/refused completion & model substitution produce no recipe. `preserve`/`review` require explicit null recipe. Model confidence is uncalibrated; it is not an acceptance score. Input, current Auto & candidate renders are shown side by side; highlight/black clipping observations are recorded without penalizing intentional clipping automatically. No winner is selected without held-out shoots & blinded preference ratings.

## Qualification & next scope

Rust protocol, capability, no-mutation & HTTP regression tests run through generated RightKit GitHub Actions (`cargo xtask ci`). This is experimental CLI infrastructure, not installed desktop cloud Auto. Before an app workflow: integrate per-app secure credentials via compatible published RightKit Secrets, background jobs/cancel/progress, immutable engine-owned proposals & stale-check/one-undo apply; run hidden native QA against exact CI artifacts.

Shared owner integration: RightKit shell/control/QA already run desktop hosting. Verification on 10 October 2026 found no published Rust `rightkit-llm` index entry ([crates.io index lookup](https://index.crates.io/ri/gh/rightkit-llm)). Published [`rightkit-http` 0.2.4](https://crates.io/crates/rightkit-http/0.2.4) is generic HTTP (`Request::json`, bounded response, timeout/retry policy), not multimodal or strict-output LLM client; source archive is [`rightkit-http-0.2.4.crate`](https://static.crates.io/crates/rightkit-http/rightkit-http-0.2.4.crate), checksum `6ca84322ac21fd3c326fa02d33ee123901a09cc2db9da99b2b42d2588e73cf19`. Its blocking `ureq` rustls path pulls `ring`/`cc`; optional async reqwest path also cannot satisfy RustCrypto-only TLS policy. Ember keeps existing rustls-rustcrypto fetch transport: bounded JSON POST, cancellation, deadline, no retries, privacy routing. SDK files remain unchanged; revisit after published shared owner exposes compatible LLM API & pure-RustCrypto dependency graph.

Sources: [OpenRouter model catalog](https://openrouter.ai/api/v1/models), [structured outputs](https://openrouter.ai/docs/guides/features/structured-outputs), [provider routing/privacy](https://openrouter.ai/docs/guides/routing/provider-selection), [DeepSeek vision](https://api-docs.deepseek.com/guides/vision/).

## Natural-language library search (ScaleDown experiment)

`lightcraft-cli ai search "sunset photos from Lisbon last summer, 4+ stars, picks only"` turns a typed search into a `library.filter` patch through the [ScaleDown Extract API](https://docs.scaledown.ai/api-reference/extract.md) (`POST https://api.scaledown.xyz/v1/chat/completions`, model pinned to `extract-1`, fetched 16 October 2026). ScaleDown models are text-only, so this is the only Ember use: no photo ever leaves the process.

```
SCALEDOWN_API_KEY=... lightcraft-cli ai search "raw files from the X-T5 rated 3 or below" [--today 2026-10-16]
lightcraft-cli ai search "beach photos of Maria in 2025" --prepare-only
```

Privacy: only the typed search text (capped at 2,000 characters) and today's date (so relative phrases resolve) are sent. No photos, no catalog contents, no metadata, no file names. `--prepare-only` prints the exact request body and makes no network call. The key is read from `SCALEDOWN_API_KEY` only, goes only into the `x-api-key` header, is never logged, serialized, or placed in a URL, body or error, and a response that echoes it is discarded. Requests have a 20 s deadline, a 64 KiB response cap, a cancel flag, and no retries; HTTP 402 reports "ScaleDown credits exhausted".

Output: the extracted entities (confidence below 0.5 dropped) and a `library.filter` patch. Entity to `Filter` mapping (`crates/catalog/src/query.rs`): `keyword` to `keyword` (extra keywords to `text`), `place` to `text` (matches location), `person`, `camera`, `lens`, `date_from`/`date_to` to `dateFrom`/`dateTo` (`YYYY`, `YYYY-MM` or `YYYY-MM-DD` only), `rating` + `rating_comparison` to `rating`/`ratingOp`, `flag` to `flag` (`pick`/`reject`/`none`), `color_label` to `label`, `file_type` to `kind` (`raw`/`image`/`video`), `edited` to `edited`. Values that cannot match a real filter value are dropped. The patch resets every field it manages, so applying it replaces the previous search. There is no app UI yet; pass the patch to `library.filter`.
