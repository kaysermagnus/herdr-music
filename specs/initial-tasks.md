# Tasks: herdr-music

Ordered for test-driven implementation. For every task marked **[TDD]**:
write the failing test at the stated seam first, then implement until green,
then refactor. Seams: `MusicProvider` trait (primary), Spotify adapter base
URL (internal, stub-server tests only).

## 1. Scaffold

- [ ] `cargo init` the `herdr-music` crate (binary, edition 2021)
- [ ] `Cargo.toml` with the allowed dep set only: `ratatui`, `crossterm`,
      `reqwest` (`blocking`, `json`), `serde`, `serde_json`, `base64`,
      `sha2`, `rand`, `dirs`, `anyhow`, `url`
- [ ] `.gitignore` (`/target`, `*.log`), module skeleton:
      `main.rs`, `provider.rs`, `config.rs`, `spotify/{mod,auth,client}.rs`,
      `app.rs`, `tui.rs`, `cli.rs`
- [ ] `herdr-plugin.toml` skeleton (pane + actions; refined in task 12)
- [ ] Verify `cargo build` and `cargo test` run green on an empty suite

## 2. Domain types and the provider seam

- [ ] **[TDD]** `Playback`, `Track`, `PlaybackStatus` data types; pure
      helpers: `elapsed_at(now)`, `seek_clamped(delta, progress, duration)`
      — unit tests for interpolation math and clamping at `[0, duration]`
- [ ] `MusicProvider` trait exactly as spec'd: `name`, `playback`, `play`,
      `pause`, `next`, `previous`, `seek`, provided `seek_relative`
- [ ] `ProviderError` taxonomy: `NotAuthenticated`, `NoActiveDevice`,
      `PremiumRequired`, `Api(String)` — drives TUI status-line messages
- [ ] `FakeProvider` in test support: scripted `playback()` responses +
      recorded command log for assertions

## 3. Config and state directories

- [ ] **[TDD]** `config_dir()`/`state_dir()` resolution — tests assert
      `HERDR_PLUGIN_CONFIG_DIR`/`HERDR_PLUGIN_STATE_DIR` env wins, then the
      platform fallback via `dirs`, isolated per-test via temp env
- [ ] `config.toml` parsing: `spotify_client_id`; env var
      `HERDR_MUSIC_SPOTIFY_CLIENT_ID` takes precedence — table-driven tests

## 4. PKCE helpers (pure)

- [ ] **[TDD]** `generate_verifier()` — 64-char URL-safe verifier, charset
      assertions
- [ ] **[TDD]** `challenge(verifier)` — equals `base64url(sha256(verifier))`
      checked against a known RFC 7636-style vector
- [ ] **[TDD]** `expiry_from(now, expires_in)` — refresh margin of 60s

## 5. Token store

- [ ] **[TDD]** JSON round-trip: write → read restores access/refresh/expiry
- [ ] **[TDD]** file written with `0600`; missing file → `None`; corrupt
      file → error, not panic (temp-dir tests)

## 6. Spotify adapter — API client

- [ ] Stub HTTP server on `127.0.0.1` in tests (raw `TcpListener` or a
      dev-dep like `tiny_http`); adapter takes `base_url` (internal seam)
- [ ] **[TDD]** `playback()` — fixture JSON → `Playback` (track, artists,
      album, duration, progress, is_playing, device name); `204` → `None`
- [ ] **[TDD]** transport commands send the right request: `PUT play`,
      `PUT pause`, `POST next`, `POST previous`, `PUT seek?position_ms=`
- [ ] **[TDD]** `401` → refresh token exchange → retry once; refresh
      failure → `NotAuthenticated`
- [ ] **[TDD]** `404` → `NoActiveDevice`; `403` → `PremiumRequired`
- [ ] Adapter plugs token store + client id from task 3–5 config

## 7. OAuth login flow

- [ ] Loopback listener on `127.0.0.1:8888` (port configurable), parses
      `code`/`state`/`error` from the callback query
- [ ] **[TDD]** callback query parsing and `state` mismatch rejection —
      pure-function tests
- [ ] `auth login`: verifier+challenge+state, open browser via `open`
      (macOS)/`xdg-open` (Linux), await callback, exchange code, persist
      tokens; prints success or the Spotify error
- [ ] `auth status` — prints connected account/provider or "not connected"
- [ ] `auth logout` — deletes token file

## 8. App controller (provider-agnostic core)

- [ ] **[TDD]** key→command mapping and controller state machine via
      `FakeProvider`: toggle semantics by `is_playing`, seek ±10s
      clamping, optimistic progress update after a command, error →
      status-line message mapping
- [ ] **[TDD]** "not authenticated" and "nothing playing" rendering states
- [ ] Worker thread: mpsc command queue + ~2s poll tick; shutdown on `q`
      without hanging joins

## 9. CLI dispatch (`ctl`, `status`)

- [ ] **[TDD]** argv parse → command enum; unknown arg → usage + exit 2
- [ ] `ctl` maps `stop`→`pause()` (command layer, per spec), `forward`/
      `rewind`→`seek_relative(±10s)`; one-line result on stdout; exit 1
      with the `ProviderError` message on failure

## 10. TUI

- [ ] **[TDD]** frame rendering on ratatui `TestBackend`: provider line,
      title/artist/album, progress bar, elapsed/total, play-state glyph,
      status line, keybinding footer — snapshot/buffer assertions
- [ ] Wire controller + worker to crossterm events, ~100ms tick
- [ ] Manual check inside a real `herdr plugin pane open` split

## 11. Manifest and Herdr integration

- [ ] `herdr-plugin.toml` final: id `herdr-music`, `platforms`
      `["linux","macos"]`, `[[build]] cargo build --release`,
      `[[panes]] player` (split), `[[actions]]` open/play-pause/next/
      previous/forward/rewind/stop/spotify-login
- [ ] `herdr plugin link` registers; `plugin action list`, `action invoke`
      smoke checks

## 12. Docs

- [ ] `README.md`: prerequisites (Rust, Spotify Developer app, redirect URI
      `http://127.0.0.1:8888/callback`, Premium note), config file/env var,
      install/link, keybinding snippet for `config.toml`
