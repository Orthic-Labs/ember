# lightcraft-fetch

Native model downloads (L0, Apache-2.0). The transfer is the RightKit owner's: `rightkit-models` `download_resumable` (vetted HTTPS through `rightkit-http`: public-web address policy, per-hop redirect checks, OS trust store; `<name>.part` plus its resume record, strong-ETag byte-range resume, bounded retries, SHA-256 check, atomic rename into place). This crate keeps what the owner leaves to the app. The wasm32 crate is empty.

Call `download` with bounded file specifications and ordered mirrors, or `download_url` for an exact catalog URL (including its query). Each file has a local filename independent of the URL, a size cap and optional pinned size/SHA-256. Mirrors are tried in order, each against the same pin; a file that fails its check is deleted, never kept. Progress is reported across files and cancellation is checked during reads. `discard_partial` removes a partial file and its resume record. Run on a worker thread. Tokens are off by default, sent only to the mirror's own host over https, and never forwarded to a different redirect host. A redirect from https to http is refused. A partial file left by an earlier build (no resume record) is replaced by a complete verified one; a model already in place and verified is never fetched again.

The `http` module is the small bounded client for map tiles and AI calls (not model downloads), also on `rightkit-http`.

Tests use local synthetic HTTP servers for redirects, resume, truncation, cancellation, stalls, hashes, size limits and hostile specifications; they make no internet requests. A loopback server is reachable only with the `testing` feature (this crate's tests, and the engine's as a dev-dependency); no product build enables it.
