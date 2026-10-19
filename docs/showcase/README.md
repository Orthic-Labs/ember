# README showcase

Existing `docs/images/` screenshots show upstream LightCraft before Ember branding. They remain historical references; regenerate captures from a qualified Ember revision when available.

The screenshots in `docs/images/` are captured by driving Ember over its control channel.

```sh
# public-domain set (downloaded into the gitignored corpus/, see assets/ATTRIBUTION.md for sources)
cargo run --release -p lightcraft -- --memory --control 7980 corpus/images/pd &
python3 docs/showcase/run.py docs/showcase/pd-edits.jsonl

# procedural demo library
cargo run --release -p lightcraft -- --memory --control 7980 &
python3 docs/showcase/run.py docs/showcase/demo.jsonl

# compress for the repo (macOS)
for f in docs/images/*.png; do sips -s format jpeg -s formatOptions 82 "$f" --out "${f%.png}.jpg" && rm "$f"; done
```

`run.py` sends each JSON line (`{"method":…,"params":…}` or `{"sleep": seconds}`) to `127.0.0.1:7980`. It authenticates first with the app's per-launch secret, read from `$LIGHTCRAFT_CONTROL_TOKEN` or the owner-only token file (`<settings folder>/control/7980.token`); pass another port as the second argument.
