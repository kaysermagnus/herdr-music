# herdr-music

Herdr plugin: Rust TUI music controller. Every music service sits behind the
`MusicProvider` trait (`src/provider.rs`); Spotify is the first adapter.

## Workflow rules

Each step runs in its own subagent profile from `.devin/agents/`, which pins
the model for that step: `spec-writer` (Fable), `coder` (SWE-2), `reviewer`
(Opus). Run them in the foreground so their tools can prompt for approval.

1. **Spec first for every new feature.** Before writing code, the
   `spec-writer` subagent writes `specs/<feature-name>.md` with the same
   sections as `specs/initial.md`. Show it to the user and start coding only
   once they approve. Bug fixes and review fixes skip this step.
2. **Code in the `coder` subagent.** Hand it the approved spec, or the review
   fixes the user chose; it works test-first and leaves changes uncommitted.
3. **Review before every commit.** The `reviewer` subagent runs
   `ponytail-review` on the uncommitted change. Show the user its report,
   have `coder` apply the fixes they choose, re-run `cargo test`, then
   commit. The commit is done when tests pass and every chosen fix is in.

## Testing

- Test-first, at the highest seam: logic above the trait runs against
  `FakeProvider` (`provider::testing`); the Spotify adapter runs through the
  same trait against the stub HTTP server in `tests/spotify_client.rs`.
- TUI: `draw` and `click` share `regions()`, so new clickable elements get a
  test that the glyph is drawn where its click maps.
- Before a commit: `cargo fmt && cargo test`; for a live run, `cargo build
  --release` (the linked plugin runs `target/release/herdr-music`).

## Gotchas

- Run `source "$HOME/.cargo/env"` first in a fresh shell; cargo is not on the
  default PATH.
- Spotify answers `411 Length Required` to PUT/POST without
  `Content-Length: 0`. A stub server accepts either, so keep the test that
  asserts the header.
- To run the binary live outside Herdr, set `HERDR_PLUGIN_CONFIG_DIR=~/.config/herdr/plugins/config/herdr-music`
  and `HERDR_PLUGIN_STATE_DIR=~/.local/state/herdr/plugins/herdr-music`.
- To remove temporary edits, back the file up first and restore from that
  backup; `git checkout -- <file>` also discards uncommitted work in it.
- Commit messages containing backticks: pass them via
  `git commit -F - <<'EOF'`, since `-m "..."` breaks on them.
