# Spec: herdr-music — TUI music controller plugin for Herdr

## Problem Statement

While working inside Herdr terminal workspaces, controlling music playback
requires leaving the terminal: reaching for a phone, switching to a desktop
app, or using OS media keys that give no feedback about what is playing.
There is no way to see the current song/album or drive playback (play, pause,
stop, next, previous, seek) from a pane next to the work in progress.

## Solution

A Herdr plugin, written in Rust, that provides a TUI media controller inside
a Herdr pane. It connects to the user's streaming account via OAuth, shows
the current song, artist, album, and playback progress, and exposes basic
transport controls: play, pause, stop, next, previous, and ±10 second seek.

The codebase is organized around a provider interface (a `MusicProvider`
trait) so that additional streaming services can be added later as adapters
without touching the TUI or command surface. Spotify is the first provider
adapter.

Because the plugin lives in a terminal multiplexer alongside other work, it
targets low memory consumption and a small footprint: a single static
binary, a minimal dependency tree, no async runtime (standard threads +
blocking HTTP), and cheap idle behavior.

The plugin is usable two ways:

- **TUI pane** — opened via `herdr plugin pane open`, default placement
  `split`, showing now-playing information and accepting transport keys.
- **Herdr actions** — headless `ctl` subcommands (play-pause, next,
  previous, forward, rewind, stop) invocable via
  `herdr plugin action invoke` and bindable to keys in the user's Herdr
  `config.toml`, so playback can be controlled without opening the TUI.

## User Stories

Authentication and account connection:

1. As a Herdr user, I want to connect my Spotify account through my browser,
   so that the plugin can control playback on my behalf.
2. As a Herdr user, I want the OAuth flow to use PKCE without a client
   secret, so that the plugin can be distributed as a public binary without
   embedded credentials.
3. As a Herdr user, I want my tokens persisted between sessions, so that I
   only have to log in once.
4. As a Herdr user, I want expired access tokens refreshed automatically
   using the stored refresh token, so that playback control keeps working
   without re-authenticating.
5. As a Herdr user, I want to check my connection status, so that I know
   which account/provider is connected before controlling playback.
6. As a Herdr user, I want to log out and remove stored tokens, so that I
   can disconnect the account or switch users.
7. As a Herdr user, I want tokens stored outside the plugin's source
   directory (in the Herdr plugin state dir), so that reinstalls and
   updates never delete or leak my credentials.
8. As a Herdr user, I want a clear message when the plugin is not
   authenticated, so that I know to run the login action instead of seeing a
   generic failure.

Now-playing display:

9. As a Herdr user, I want to see the current track title, so that I know
   what is playing.
10. As a Herdr user, I want to see the artist name(s), so that I can
    identify who performs the current track.
11. As a Herdr user, I want to see the album name, so that I can identify
    the release the track belongs to.
12. As a Herdr user, I want to see elapsed time, total duration, and a
    progress bar, so that I know where in the track playback is.
13. As a Herdr user, I want the display to show whether playback is playing
    or paused, so that the state is unambiguous at a glance.
14. As a Herdr user, I want the display to refresh periodically, so that it
    stays in sync when playback changes outside the TUI (phone, desktop
    app).
15. As a Herdr user, I want to see which provider is connected, so that the
    interface stays meaningful as more providers are added.
16. As a Herdr user, I want to see a "nothing playing" state when there is
    no active playback, so that the display is honest rather than stale.

Transport controls:

17. As a Herdr user, I want a play/pause toggle on one key, so that the most
    common action takes one keystroke.
18. As a Herdr user, I want explicit pause, so that I can halt playback.
19. As a Herdr user, I want a stop action, so that I can end playback
    cleanly (mapped to pause where the provider API has no true stop).
20. As a Herdr user, I want next and previous track controls, so that I can
    move through a queue or album.
21. As a Herdr user, I want to seek forward 10 seconds on one key, so that I
    can skip ads, intros, or silences.
22. As a Herdr user, I want to seek backward 10 seconds on one key, so that
    I can re-hear a passage.
23. As a Herdr user, I want seeks clamped to the track bounds, so that
    seeking past the start/end produces a defined result instead of an
    error.
24. As a Herdr user, I want immediate visual feedback after a control
    command, so that the display does not wait for the next poll to feel
    responsive.
25. As a Herdr user, I want a manual refresh key, so that I can force a
    re-sync with the provider on demand.

Herdr integration:

26. As a Herdr user, I want the player to open in a split pane by default,
    so that it sits beside my work like a mini-player.
27. As a Herdr user, I want the pane placement overridable at open time
    (overlay, popup, tab, zoomed), so that I can use it transiently when I
    prefer.
28. As a Herdr user, I want plugin actions for each transport control, so
    that I can drive playback without opening the TUI.
29. As a Herdr user, I want to bind actions to keys in my Herdr config, so
    that e.g. `prefix+p` toggles play/pause from anywhere.
30. As a Herdr user, I want the plugin to work on macOS and Linux, so that
    it follows me across machines.
31. As a Herdr user, I want `herdr plugin link` to work during development,
    so that iterating on the plugin requires no publish step.
32. As a Herdr user, I want a `[[build]]` step in the manifest
    (`cargo build --release`), so that GitHub installs compile the binary
    automatically.

Footprint and behavior:

33. As a Herdr user, I want the plugin to be a single small binary with a
    minimal dependency tree, so that it stays cheap to build, install, and
    keep resident.
34. As a Herdr user, I want idle TUI polling to be infrequent (~2s) and to
    pause when the pane is closed, so that the plugin does not burn CPU,
    memory, or API rate limits.
35. As a Herdr user, I want API calls off the UI thread, so that a slow
    network never freezes the interface.

Error handling:

36. As a Herdr user, I want a clear message when no playback device is
    active, so that I know to start Spotify somewhere first.
37. As a Herdr user, I want a clear message when the account lacks Premium,
    so that I understand why control is rejected.
38. As a Herdr user, I want errors shown inside the TUI (status line) rather
    than crashing the pane, so that a transient API failure does not lose
    my layout.

## Implementation Decisions

### Packaging and module layout

- One Cargo package, `herdr-music`, producing a single binary of the same
  name. The repository root doubles as the plugin directory and contains
  `herdr-plugin.toml`.
- Plugin id: `herdr-music`, matching the repo and binary. `name = "Music"`,
  `min_herdr_version = "0.7.0"`, `platforms = ["linux", "macos"]`.
- `[[build]]` command: `cargo build --release` so `herdr plugin install`
  from GitHub compiles automatically. `plugin link` uses the working tree's
  existing build.

### Binary interface

Single binary, subcommand-dispatched (no clap — argv matched by hand to
keep the dep tree small):

- `herdr-music tui` — launch the TUI (used by the `[[panes]]` entrypoint).
- `herdr-music ctl <cmd>` — one-shot control for actions/keybindings:
  `play-pause | play | pause | stop | next | previous | forward | rewind`.
  Prints a short result line; non-zero exit on failure.
- `herdr-music auth login|status|logout` — OAuth lifecycle.
- `herdr-music status` — print current playback as one line (for scripts).

### The seam: `MusicProvider`

- A single provider-agnostic trait is the only public interface the TUI,
  `ctl`, and `status` commands know. Everything above the seam is
  provider-agnostic; each service is an adapter behind it.
- Interface surface (deliberately small):
  - `playback()` — current `Option<Playback>` (track, artists, album,
    duration, progress, playing state, device name).
  - `play()`, `pause()`, `next()`, `previous()` — transport verbs.
  - `seek(position)` — absolute seek; relative ±10s is a provided method
    implemented as `playback()` → clamp → `seek()`.
  - `name()` — display name of the provider.
- "Stop" is deliberately not a trait method: Spotify's API has no stop; the
  `stop` ctl command maps to `pause()` at the command layer.
- `Playback`/`Track` are plain data types owned by the core, not by any
  adapter.

### Spotify adapter

- Hand-rolled client over `reqwest` (blocking) + `serde_json`. No rspotify,
  no oauth2 crate.
- Auth: Authorization Code with PKCE against `accounts.spotify.com`. The
  binary binds `http://127.0.0.1:<port>/callback` (default port 8888,
  overridable), opens the system browser via `open`/`xdg-open`, validates
  `state`, and exchanges the code for tokens.
- Scopes: `user-read-playback-state`, `user-modify-playback-state`,
  `user-read-currently-playing`.
- Client id resolution order: `HERDR_MUSIC_SPOTIFY_CLIENT_ID` env →
  `config.toml` (`spotify_client_id`) in the config dir. Documented manual
  step: user creates a Spotify Developer app and registers the redirect
  URI.
- Token store: JSON file under `HERDR_PLUGIN_STATE_DIR` (fallback:
  platform state dir when the env var is absent, e.g. plain `cargo run`
  outside Herdr). Written with `0600` permissions. Access tokens refreshed
  via the refresh token when within 60s of expiry; a 401 triggers one
  refresh-and-retry.
- Internal test seam: the adapter's HTTP layer takes a base URL, so tests
  point it at a stub server on localhost.

### Concurrency model

- No async runtime. Two threads: the TUI thread (crossterm event poll with
  a ~100ms tick) and a worker thread that owns the provider and executes
  commands + periodic playback polls (~2s), communicating over
  `std::sync::mpsc`. UI stays responsive; the worker is the only place
  blocking HTTP happens.

### TUI

- `ratatui` + `crossterm` only. Single screen: provider line, track title,
  artist, album, progress bar with elapsed/total, play-state indicator,
  error/status line, keybinding footer.
- Keys: `space` play/pause toggle, `s` stop, `n` next, `p` previous,
  `→`/`l` forward 10s, `←`/`h` rewind 10s, `r` refresh, `q` quit.
- Progress is interpolated between polls from the last known
  `progress + elapsed wall time` when playing, so the bar moves smoothly
  without extra API calls.

### Manifest surface

- `[[panes]]` entry `player` → `["./target/release/herdr-music", "tui"]`,
  `placement = "split"`, title "Music".
- `[[actions]]`: `open` (invokes `herdr plugin pane open` for `player`),
  `play-pause`, `next`, `previous`, `forward`, `rewind`, `stop` (each →
  `ctl`), and `spotify-login` (→ `auth login`). All `contexts = ["workspace"]`.

### Dependency policy (footprint)

- Allowed: `ratatui`, `crossterm`, `reqwest` (`blocking`+`json` only),
  `serde`/`serde_json`, `base64`, `sha2`, `rand`, `dirs`, `anyhow`, `url`.
- Excluded deliberately: `rspotify`, `oauth2`, `clap`, `open`/`webbrowser`
  crates.
- Exception (`specs/airplay-receiver.md`): `tokio` is permitted inside
  `airplay::receiver` only — the shairplay RAOP server is tokio-based.
  Nothing above the `MusicProvider` seam gains an async signature; the
  two-thread `std::sync::mpsc` design is unchanged.

## Testing Decisions

What makes a good test here: tests cross the `MusicProvider` seam and
assert externally observable behavior — state returned by `playback()`,
the HTTP requests a stub server receives, rendered frame contents, CLI
exit codes/stdout. They never assert private fields, internal call order,
or implementation structure.

- **One primary seam.** `FakeProvider` (an in-memory adapter in test code)
  stands in for Spotify for everything above the seam. No second adapter
  is added to production code just for testing.
- **App/controller logic** (key → command mapping, toggle semantics, seek
  clamping to `[0, duration]`, progress interpolation, "not authenticated"
  and "nothing playing" states) is unit-tested against `FakeProvider`.
- **Spotify adapter** is tested through the same `MusicProvider` trait
  against a stub HTTP server bound to `127.0.0.1` in tests (the adapter's
  internal base-URL seam). Cases: playback JSON → `Playback` mapping,
  each transport command's request shape, 204-vs-error handling,
  401 → refresh → retry, no-active-device and Premium-required error
  mapping.
- **PKCE helpers** are pure functions: challenge equals
  `base64url(sha256(verifier))`, verifier length/charset, expiry
  computation from `expires_in`. Straight unit tests.
- **Token store**: temp-dir tests for write/read round-trip, file
  permissions, and missing/corrupt file behavior.
- **CLI dispatch**: `ctl` argument parsing and exit codes via
  `assert_cmd`-style integration tests only if cheap; otherwise dispatch is
  kept thin enough to cover through provider-level tests.
- **Workflow**: strict TDD — each task in `initial-tasks.md` writes the failing
  test at the seam first, then the implementation, then refactor.
- **Prior art**: none — greenfield project. This spec establishes the
  convention: `tests/` for seam-level integration, `#[cfg(test)]` modules
  for pure functions, hand-rolled fakes over mock frameworks where
  practical.

## Out of Scope

- Actual audio playback or streaming — this is a remote control for an
  existing player/device, not a player.
- Other providers (Apple Music, YouTube Music, MPD, local files). The
  `MusicProvider` seam anticipates them; none is implemented.
- Volume control, shuffle/repeat toggles, queue management, playlist or
  library browsing, search, album art.
- Windows support (manifest declares `linux`, `macos` only).
- Marketplace submission, GitHub release plumbing, and writing keybindings
  into the user's `config.toml` (documented, user-applied).
- Creating the Spotify Developer application — a manual prerequisite for
  the user.
- Spotify Free accounts: Web API playback control requires Premium; the
  plugin surfaces this as a clear error rather than working around it.

## Further Notes

- Prerequisites before apply: Rust toolchain (rustup — not yet installed;
  install was interrupted), and a Spotify app client id + registered
  `http://127.0.0.1:8888/callback` redirect URI.
- Herdr plugin docs (v0.9.5) confirm: plugin = dir + `herdr-plugin.toml`;
  panes are argv commands; env provides `HERDR_PLUGIN_STATE_DIR`,
  `HERDR_PLUGIN_CONFIG_DIR`, `HERDR_BIN_PATH`; actions bindable via
  `[[keys.command]]` with `type = "plugin_action"`.
- Development loop: `cargo build --release` → `herdr plugin link <dir>` →
  `herdr plugin pane open --plugin herdr-music --entrypoint player`.
