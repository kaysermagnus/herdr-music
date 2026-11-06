# Spec: local playback — stream Spotify to this laptop from the player pane

## Problem Statement

herdr-music is a remote control: every transport verb behind `MusicProvider`
is forwarded to whatever Spotify Connect device is already active, and when
none is, the user sees "no active device — start playback on a device first".
On a laptop with no Spotify desktop app running, that means leaving the
terminal to start a player somewhere else before the pane is useful at all.
The Spotify Web API carries no audio and the Web Playback SDK is
browser-only, so nothing in the current stack can produce sound.

## Solution

While the player pane is open, the plugin runs a Spotify Connect engine
(librespot, MIT) as a child process that registers this laptop as a device
named for the plugin. The user picks a playback target from a device picker
in the TUI: the local device or any other Connect device on the account.
Choosing a device issues Spotify's transfer-playback call through the
existing Web API client; everything else (now-playing, transport, seek) keeps
working unchanged because the engine is just one more device to Spotify.

The engine lives and dies with the pane: spawned by the worker thread when
the TUI starts, killed when it exits, never left running in the background.
`ctl`, `status`, and the Herdr actions stay headless and never start it.

librespot is compiled during `herdr plugin install` by platform-gated
`[[build]]` steps, pinned to one version, so the user installs nothing by
hand on macOS, Linux, or Windows beyond the Rust toolchain the plugin
already requires (on Windows that is the MSVC toolchain). On macOS and
Windows the engine plays through its built-in rodio backend (CoreAudio and
WASAPI respectively, no extra libraries). On Linux it is built without the
ALSA-dependent backend and instead writes PCM to a pipe that the plugin
feeds into whichever of `pw-play`, `paplay`, or `aplay` is already present,
so no development headers are needed at build time and no audio library is
required at runtime. TLS is rustls with bundled webpki roots on all three,
so no OpenSSL headers anywhere.

The plugin itself is Linux/macOS-only today, so this feature carries a
minimal Windows port of the plugin (manifest platforms, shell-free `open`
action, browser launch, token-file permissions, engine path suffix) as a
prerequisite subsection.

Footprint goals hold: no new crate dependencies, no async runtime. Process
management uses `std::process` plus the OS's own process tools (`kill`,
`tasklist`/`taskkill`), readiness is detected through the Web API device
list, and all engine I/O is plumbed with standard pipes and threads.

## User Stories

Device selection:

1. As a Herdr user, I want this laptop to appear as a Spotify Connect device
   whenever the player pane is open, so that I can play music here without
   launching another Spotify client.
2. As a Herdr user, I want a device picker in the TUI listing this laptop
   and every other Connect device on my account, so that I can choose where
   music plays.
3. As a Herdr user, I want the picker to mark the currently active device and
   to label the local one clearly, so that I know what I am switching from
   and to.
4. As a Herdr user, I want selecting a device to transfer playback there and
   keep playing if it was playing, so that switching is seamless.
5. As a Herdr user, I want to keep using other devices as before even when
   the local engine cannot start, so that a local-audio problem never costs
   me remote control.

Lifecycle:

6. As a Herdr user, I want the engine to stop when I close the pane, so that
   closing the pane means no audio and no process left behind.
7. As a Herdr user, I want a stale engine left by a crashed pane to be
   cleaned up the next time I open the player, so that crashes never leave
   orphan processes playing or holding the device name.
8. As a Herdr user, I want the pane to open promptly and show "starting
   local playback…" while the engine registers, so that a slow registration
   never blocks the now-playing display.
9. As a Herdr user, I want to be able to restart the local engine from the
   picker if it died, so that recovering does not require closing the pane.

Errors and status:

10. As a Herdr user, I want a clear status message when the engine fails to
    start (binary missing, login rejected, early exit), including the
    engine's last error line, so that I can tell what to fix.
11. As a Herdr user on Linux, I want a clear message when no audio player
    (`pw-play`, `paplay`, `aplay`) is found, so that I know what to install.
12. As a Herdr user who logged in before this feature, I want to be told to
    re-run `auth login` to grant the streaming permission, rather than seeing
    an opaque engine failure.
13. As a Herdr user without Premium, I want the existing Premium message,
    since librespot has the same requirement as the Web API.

Install and configuration:

14. As a Herdr user, I want `herdr plugin install` to build the engine for me
    on macOS, Linux, and Windows, so that there is no manual step beyond the
    Rust toolchain the plugin already needs.
15. As a Herdr user on Windows, I want the plugin itself (pane, actions,
    login) to work there, so that local playback is not the only thing that
    does.
16. As a Herdr user, I want to be able to disable local playback in
    `config.toml` and to rename the local device, so that the plugin can stay
    a pure remote control on machines where I never want audio.
17. As a Herdr user, I want the README to credit librespot and its MIT
    license, so that attribution is honest.

## Implementation Decisions

### Scope of change by module

- `provider`: new `Device` data type (id, name, is_active) owned by the core,
  three new trait methods — `devices()`, `transfer(device_id)`, and the
  local-engine pair `start_local(name)` / `stop_local()` — and a new
  `ProviderError::LocalPlayback(String)` variant for engine-side failures.
  `start_local`/`stop_local` have default implementations that return
  `ProviderError::Api("local playback not supported")` and `Ok(())`, so
  future providers without an engine need no changes. `FakeProvider` in
  `provider::testing` gains a configurable device list and records
  `transfer:<id>`, `start_local`, `stop_local` in `calls`.
- New core module `engine`: provider-agnostic child-process management —
  spawn with captured stderr, optional stdout-to-sink pumping, kill-on-drop,
  bounded ring of last stderr lines, pidfile write/reap, and a "wait until
  predicate or exit" helper that takes the readiness check as a closure.
  Knows nothing about Spotify or librespot.
- New Spotify submodule `spotify::local`: builds the librespot argv (binary
  path, access token, device name, device type `computer`, backend per OS,
  discovery disabled, audio and credential caches disabled) as a pure
  function of the target OS, picks the Linux audio sink, and implements
  readiness as "our device name appears in `devices()`" — the same request
  the picker uses.
- `spotify` adapter: implements `devices()` (`GET /me/player/devices`) and
  `transfer()` (`PUT /me/player` with a JSON body). The adapter's request
  helper currently sends `Content-Length: 0` on every non-GET; it gains a
  JSON-body variant for transfer and keeps the empty-body behavior for the
  existing verbs (the 411 gotcha still applies to those).
- `spotify::auth`: `streaming` added to the scope list. `StoredTokens` gains
  a `scopes` field (serde default: empty) written at login, so the adapter
  can tell pre-feature tokens apart from current ones without a network call.
  The browser launch gains a Windows arm (see the Windows port subsection).
- `cli` / `main`: new `open` subcommand (a `Requested` variant) that runs
  `$HERDR_BIN_PATH plugin pane open --plugin herdr-music --entrypoint
  player` through `std::process`, replacing the `sh -c` action command.
- `app`: new commands `Devices` (open/refresh the picker), `SelectDevice(id)`,
  `StartLocal`; `Snapshot` carries the device list, the local-engine status
  (not started / starting / ready / failed(reason) / disabled(reason)), and
  whether the picker is open. Controller logic stays provider-agnostic.
- `tui`: a `d` key toggles the picker; `↑/↓`/`j/k` move, Enter selects, Esc
  closes; clicking a picker row selects it. Picker rows join `regions()` so
  `draw` and `click` share geometry. The status line shows engine state
  while the picker is closed; the footer changes while it is open.
- `config`: two keys read from `config.toml` through the same minimal
  line parser as `spotify_client_id` — `local_playback` (bool, default
  true) and `local_device_name` (default "Herdr Music").
- `herdr-plugin.toml`: `windows` added to `platforms`; the `open` action
  points at the new subcommand; platform-gated `[[build]]` entries (below).
- `README`: librespot attribution, the Linux audio-player note, and the
  Windows requirement (Rust MSVC toolchain — already needed for the plugin).
- `.github/workflows/ci.yml`: new — a GitHub Actions matrix over
  `ubuntu-latest`, `macos-latest`, `windows-latest` running `cargo fmt
  --check` and `cargo test` on the stable toolchain (see CI below).

### Windows port of the plugin (prerequisite)

The plugin is declared for `linux`/`macos` only, and three spots are
Unix-shaped. Herdr runs natively on Windows (ConPTY), launches plugin
commands with no shell, and there `bash` resolves to WSL, so every manifest
command must be valid Windows argv on its own.

- Manifest: `platforms = ["linux", "macos", "windows"]`. The `open` action
  currently runs `sh -c "$HERDR_BIN_PATH" plugin pane open …`, which cannot
  work there. It becomes `herdr-music open`, a subcommand that reads
  `HERDR_BIN_PATH` from the environment and spawns Herdr via `std::process`,
  so one action id serves all OSes — Herdr rejects duplicate action ids
  even across disjoint `platforms`, so per-OS `open` variants are not an
  option. The per-entry `platforms` field is used only on the `[[build]]`
  entries that compile the engine.
- Browser launch in `spotify::auth`: a `cfg(windows)` arm runs
  `rundll32 url.dll,FileProtocolHandler <url>`. Chosen over `cmd /C start
  "" <url>` because `cmd` treats the `&` separating OAuth query parameters
  as a command separator unless the URL is re-quoted/escaped through cmd's
  rules, and over `explorer <url>` because explorer's exit status and
  handling of long query strings are undocumented. `rundll32` takes the URL
  as a plain argv element and hands it to the default browser.
- Token file in `spotify::tokens`: the `0600` chmod stays `cfg(unix)`. On
  Windows the file lives under the per-user profile directory
  (`HERDR_PLUGIN_STATE_DIR`, or `dirs::data_local_dir` as `config` already
  falls back to) with default ACLs, which are user-private. This is
  accepted; no ACL calls are added.
- Engine binary path: built with `std::env::consts::EXE_SUFFIX` so it
  resolves to `librespot.exe` on Windows.
- Risk to check first in the manual Windows run: whether Herdr accepts the
  existing pane/action command `./target/release/herdr-music` (no `.exe`,
  forward slashes) on Windows. If not, the pane and action commands need a
  Windows-gated variant — but since action ids cannot be duplicated across
  platforms, that would mean a command form that works everywhere (to be
  decided from what the run shows).
- Implementation order: the port lands before the engine work so that each
  Windows-specific behaviour is verifiable on its own.

### Engine process lifecycle

- Owner: the worker thread owns the provider; the adapter holds the engine
  handle behind the same kind of `Mutex` it already uses for the token cache.
  Dropping the provider at worker exit drops the handle, which kills the
  child. Normal quit, `Esc`, and unwinding panics all go through this path
  (same pattern as the terminal guard).
- Startup order: on pane open the worker sends the first `Refresh` as today,
  then `StartLocal`. Start is skipped — with a `disabled(reason)` status —
  when `local_playback = false`, the engine binary is missing, the stored
  tokens lack the `streaming` scope, or (Linux) no audio player is on PATH.
  Skipping never affects remote control.
- Token: librespot authenticates once at startup with the access token and
  keeps its own session afterwards, so the engine does not need refreshes
  while running. Every spawn (initial or restart from the picker) obtains
  the token through the adapter's existing access-token path, which refreshes
  when inside the expiry margin.
- Readiness: the engine's stdout is audio on Linux, so readiness is not read
  from the process. `start_local` polls `devices()` every second until a
  device with the configured name appears (ready) or the child exits
  (failed) or 20 seconds pass (failed: timed out). Polling blocks only the
  worker; the UI thread keeps rendering with the "starting" status.
- Audio on Linux: librespot runs with the pipe backend writing PCM to
  stdout; the plugin spawns the first available of `pw-play`, `paplay`,
  `aplay` with the matching raw-format arguments and connects librespot's
  stdout to the player's stdin directly (`Stdio` handoff, no copying thread).
  On macOS and Windows librespot uses its built-in rodio backend (CoreAudio
  / WASAPI) and stdout is discarded.
- Crash detection: on each worker tick the adapter checks the child with a
  non-blocking wait; an exit flips the status to `failed(last stderr line)`.
  No automatic respawn — the picker offers "This laptop (restart)".
- Shutdown, without the `libc` crate: on Unix `stop_local` runs the `kill`
  command (`kill -TERM <pid>`), waits up to two seconds for the child, then
  falls back to `Child::kill` (SIGKILL). Windows has no SIGTERM, so there
  `stop_local` goes straight to `Child::kill` (TerminateProcess). The Linux
  audio player exits on EOF when librespot dies. The pidfile is removed
  afterwards.
- Orphan prevention: the pidfile in the state dir records the engine pid and
  the plugin pid that spawned it. At every `start_local`, a stale pidfile
  whose plugin pid is dead and whose engine pid is alive is reaped before
  spawning. Liveness and reaping use OS tools, no new crates: on Unix
  `kill -0 <pid>` / `kill -9 <pid>`; on Windows `tasklist /FI "PID eq N"`
  / `taskkill /PID N /F`. Because pids are reused, the reaper also checks
  the process name (`ps -o comm= -p <pid>` on Unix, the image name column
  of `tasklist` on Windows) and only kills a pid whose name is librespot.
  `PR_SET_PDEATHSIG` was rejected: it needs the `libc` crate and is
  Linux-only.
- Engine binary location: `HERDR_MUSIC_ENGINE` env override (also the hook
  tests use for the fake engine), else `target/librespot/bin/librespot` with
  `std::env::consts::EXE_SUFFIX` appended, resolved relative to the running
  executable's `target/release` parent.

### Build changes and install-time cost

- After `cargo build --release`, two more `[[build]]` entries gated with the
  per-entry `platforms` field, no shell anywhere (Herdr runs them as plain
  argv on every OS):
  - `platforms = ["linux"]`: `cargo install librespot --version 0.8.0
    --locked --root ./target/librespot --no-default-features --features
    rustls-tls-webpki-roots`
  - `platforms = ["macos", "windows"]`: the same with `--features
    rustls-tls-webpki-roots,rodio-backend`
- TLS: librespot 0.8.0 requires exactly one of its mutually exclusive TLS
  features (`native-tls`, `rustls-tls-native-roots`,
  `rustls-tls-webpki-roots`; a compile-time check in librespot-oauth
  enforces it), so a bare `--no-default-features` does not compile.
  `native-tls` would need OpenSSL development headers on Linux;
  `rustls-tls-webpki-roots` is chosen on all three OSes for one uniform
  build with no system TLS or CA-store dependency. librespot-core's rustls
  path goes through `hyper-rustls` with the `ring` provider, so no CMake or
  NASM is pulled in either.
- Audio backend: `rodio-backend` is WASAPI on Windows and CoreAudio on
  macOS, nothing to install; on Linux rodio means ALSA headers, hence the
  pipe backend there. Discovery (`with-libmdns`) is left out everywhere:
  the engine logs in with a token, so zeroconf is dead weight and one less
  dependency tree to compile.
- Cost: a clean librespot build takes a few minutes on a laptop; installing
  with `--root` keeps only the ~15 MB binary under `target/`, not the
  intermediate artifacts. The step is idempotent, so re-installs are fast
  when the pinned version is already present.
- Pinning: the version is a literal in the manifest and mirrored as a
  constant the engine module checks against `librespot --version` at
  startup, surfacing a clear "engine version mismatch, re-run plugin
  install" status instead of a confusing argv error after an upgrade.
- librespot 0.8.0 requires Rust 1.85; the plugin's own MSRV implicitly rises
  to match (noted in the README). On Windows the toolchain is the MSVC one,
  which the plugin's own `cargo build` already requires — nothing extra.

### CI

- The repo moves to public (secrets audit already done: full history
  scanned, no tokens, secrets, or personal data beyond the noreply
  committer email; the Spotify client id has never been committed), so
  GitHub Actions is free for the private/public matrix.
- New `.github/workflows/ci.yml`: on push to `main` and on PRs, a matrix
  over `ubuntu-latest`, `macos-latest`, `windows-latest` installs stable
  Rust and runs `cargo fmt --check`, `cargo build`, and `cargo test`.
  `rust-toolchain` is not pinned; the Windows leg is what keeps the
  `cfg(windows)` arms and `tests/engine.rs` honest.
- Land order: the public flip and the CI workflow go in with (or just
  before) the Windows port change, so the port is exercised on all three
  OSes as soon as it lands.

### OAuth scope change

- Scopes become `user-read-playback-state user-modify-playback-state
  user-read-currently-playing streaming`. The device list and transfer
  endpoints are already covered by the first two.
- Tokens saved before this change carry no `scopes` field; the adapter treats
  a missing `streaming` scope as `disabled("re-run herdr-music auth login to
  enable local playback")` and does not spawn the engine. Remote control is
  unaffected, so existing users lose nothing until they re-authorize.

### TUI behavior

- Status line, picker closed: "starting local playback…" while registering;
  on success the existing "on <device>" line (Spotify reports our device
  name once playback is transferred); on failure or disabled state the reason
  in the error color, replaced by the next successful refresh only if the
  user has dismissed it by opening the picker. Exception: the
  "disabled, re-run auth login" state is silent — pre-feature users are
  not nudged on the status line and only see the re-login note inside the
  device picker.
- Picker open: one row per device, active one marked with the accent glyph,
  local one suffixed "(this laptop)" or "(this laptop — restart)" when the
  engine has failed; when the stored token lacks `streaming`, the local row
  instead reads "(this laptop — re-run auth login)" and selecting it is a
  no-op. Enter/click on a row sends `SelectDevice` (or `StartLocal` for the
  restart row) and closes the picker.
- Initial target: opening the pane does not transfer playback by itself. If
  the engine becomes ready while nothing is playing anywhere, the worker
  transfers to the local device once so that pressing play makes sound here;
  if something is already playing elsewhere, it is left alone.

## Testing Decisions

Good tests here cross a seam and assert observable behavior: calls recorded
by `FakeProvider`, requests the stub HTTP server receives, process state of a
fake engine, rendered buffer cells. Tests are written before the code they
drive.

- **Controller logic** (`app`, against `FakeProvider`): `Devices` fills the
  snapshot's device list; `SelectDevice` records `transfer:<id>` then
  refreshes; `StartLocal` records `start_local` and the resulting status
  (ready, failed, disabled) lands on the snapshot; auto-transfer happens
  once when the engine becomes ready with no playback and never when
  something is playing; a `LocalPlayback` error becomes a status message
  rather than a panic.
- **Spotify adapter** (`tests/spotify_client.rs`, through the trait against
  the stub server): `devices()` maps the device-list JSON to `Device`s;
  `transfer()` issues `PUT /me/player` with a JSON body naming the device id
  and `play: true` and a non-zero `Content-Length`; the existing assertion
  that the transport verbs still send `Content-Length: 0` stays; a 404 on
  transfer maps to `NoActiveDevice`. `start_local` with the fake engine and a
  stub that serves a device list without, then with, the local name returns
  ready and makes exactly those requests.
- **Engine process** (new `tests/engine.rs`, against a fake engine): the fake
  is the test executable itself. Tests set `HERDR_MUSIC_ENGINE` to
  `std::env::current_exe()` and `HERDR_MUSIC_FAKE_ENGINE=<mode>`; a
  dedicated helper test in the same file, run with `--exact`, sees the env
  var and acts as the engine instead of asserting anything. This keeps the
  fake portable — no committed script, nothing that needs `sh`, so the file
  runs unchanged on Linux, macOS, and Windows. Modes: sleep until killed
  (default), exit immediately with a line on stderr, ignore the polite
  termination (Unix only; Windows has no such phase). Tests: the argv the
  engine received is written to a file and matches what the pure argv
  builder produces for the current OS; dropping the handle ends the
  process; early exit reports the stderr line; shutdown ends the process
  within the deadline (Unix: TERM escalates to KILL; Windows: direct kill);
  a pidfile is written and a stale pidfile with a dead parent is reaped on
  the next start, and a stale pidfile whose pid now belongs to a differently
  named process is left alone.
- **Argv per OS** (`spotify::local`): the librespot argv builder is a pure
  function of a target-OS value, unit-tested for linux (pipe backend),
  macos and windows (rodio backend) on every host, so the per-OS flag set
  is checked without needing three machines.
- **Linux sink selection**: pure function from an ordered candidate list and
  a PATH-lookup closure to the chosen player and its argv; unit-tested for
  each candidate and for "none found".
- **Browser launch** (`spotify::auth`): the command is built by a pure
  function of target OS and URL, unit-tested for all three; the Windows case
  asserts the URL with `&` reaches the argv unchanged as one element.
- **`open` subcommand** (`cli`): parses to its `Requested` variant; the
  spawned command is built by a pure function of `HERDR_BIN_PATH`, tested for
  the exact argv, and a missing `HERDR_BIN_PATH` yields a clear error.
- **Engine path**: the default binary path ends in `EXE_SUFFIX`, asserted on
  the host.
- **Scopes**: token store round-trips the new `scopes` field, loads a
  pre-feature file as empty scopes, and the "needs re-login" check is a
  pure predicate over it. `authorize_url` test extends to assert
  `streaming`.
- **Config**: the line parser returns `local_playback` and
  `local_device_name` with their defaults when absent.
- **TUI** (`TestBackend` render + `regions()`): `d` maps to `Devices`;
  picker rows are drawn where `click` maps them, following the existing
  "glyph is drawn where its click maps" test; the active marker and "(this
  laptop)" label render; the starting/failed/disabled statuses render on
  the status line; the picker survives the tiny-pane test.
- No live librespot is run in `cargo test`; a manual checklist (install on
  a clean macOS, a clean Linux VM, and a clean Windows VM; pick the local
  device, hear audio, close pane, confirm no process remains) is recorded
  in Further Notes.
- CI runs the same suite on all three OSes (see the CI subsection), so
  `cargo test` on Windows is no longer only manual; the VM checklist
  covers what CI cannot (audio out, OAuth login, Herdr command shapes).

## Out of Scope

- Running the engine outside the pane (background daemon, `ctl` starting it,
  keeping it alive across pane reopen).
- Linking librespot as a library: it requires tokio, which the no-async-
  runtime goal rules out; the subprocess boundary is deliberate.
- Shipping prebuilt librespot binaries or vendoring its source.
- Automatic respawn after an engine crash (manual restart from the picker
  only).
- Volume control, local audio-device selection within the OS, bitrate or
  normalisation settings, local audio cache.
- Remembering the last chosen device across sessions.
- Windows beyond the minimal port above: no Windows-specific token ACLs, no
  WSL support (Herdr runs natively there), no MSYS/Cygwin shells.
- Zeroconf/Connect discovery of the local device by other Spotify apps
  (discovery is built out; the device still appears via the account's
  Connect list because it is logged in).
- Spotify Free accounts (librespot needs Premium; existing message applies).

## Further Notes

- librespot 0.8.0 (2025-11-10) is the current release on crates.io; MIT,
  MSRV 1.85. Its `--access-token` option documents the `streaming` scope
  requirement; the pipe backend writes PCM to stdout; `--onevent` only fires
  on player events, which is why readiness is detected through the device
  list instead.
- Risk: Spotify's device list can lag the engine's connection by a few
  seconds, hence the 20 s readiness window; if field reports show longer
  lags the window is a single constant in the Spotify local module.
- Risk: the credential cache is disabled so no derived credentials are
  written to disk; librespot's docs advise against this with token login
  because the token cannot be reused after expiry — irrelevant here since
  every spawn gets a fresh token, but worth re-checking if librespot changes
  its login flow.
- Risk: install-time build of several minutes may surprise users; the
  README's install section should state it. A follow-up could cache the
  binary across plugin reinstalls in the state dir.
- Licensing: the plugin stays Apache-2.0; librespot is built from crates.io
  at install time and not redistributed, so no license file is bundled, but
  README attribution is included.
- Risk: CI runs `cargo test` on all three OSes but cannot exercise audio
  output, OAuth login, or Herdr's command resolution on Windows; a
  regression there still needs the manual checklist.
- Risk (Windows, check first): Herdr may not accept
  `./target/release/herdr-music` as a pane/action command there; see the
  port subsection for the fallback reasoning.
- Post-spec upstream change (verified live 2026-11): Spotify rejects
  third-party Web API tokens at Connect/login5 with `INVALID_CREDENTIALS`,
  and even librespot's own `--enable-oauth` credentials register the
  device but get no audio keys (tracks fail to decrypt and skip). Only
  zeroconf-handshake credentials work, so the engine runs discovery
  (with-dns-sd on macOS — libmdns fails to advertise on multi-interface
  Macs — with-libmdns on Linux/Windows) + `--system-cache`: the user
  picks the device in
  their Spotify app once, credentials persist in `engine-cache/`, later
  starts reuse them. The plugin token is Web-API-only — no token or
  `streaming` gate in `start_local`. Readiness: 20 s with cached
  credentials, 180 s while the first-time app pick is pending, timeout
  message points at the app pick.
- Manual verification checklist: clean macOS, Linux, and Windows installs
  via `herdr plugin install`; `spotify-login` action opens the browser and
  completes; `open` action opens the pane; `d` shows "Herdr Music (this
  laptop)"; select it; audio plays; `q`; `pgrep librespot` (Windows:
  `tasklist | findstr librespot`) is empty; kill -9 / `taskkill /F` the
  pane process while playing, reopen, confirm the stale engine was reaped.
- Decisions confirmed with the user (formerly open questions):
  1. Auto-transfer policy: transfer to the local device automatically only
     when nothing is playing anywhere — as specced in TUI behavior.
  2. Default local device name: "Herdr Music", no hostname lookup.
  3. The multi-minute `cargo install librespot` inside
     `herdr plugin install` is acceptable; no opt-in mechanism.
  4. Pre-feature users see the re-login note only inside the device
     picker; the closed-picker status line stays silent for that state.
  5. The Windows port of the plugin lands as a separate change before
     local playback: it is committed and verified on a Windows VM first.
  6. CI is in scope: a GitHub Actions matrix over the three OSes running
     `cargo fmt --check` and `cargo test`; the repo goes public first
     (secrets audit done — see the CI subsection).
