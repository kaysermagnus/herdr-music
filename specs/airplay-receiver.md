# Spec: AirPlay receiver — stream from an iPhone, iPad or Mac to the player pane

## Problem Statement

The plugin is a remote control: it shows and drives playback that happens
somewhere else. After `specs/local-playback.md` was withdrawn (Spotify
denies audio keys to every third-party credential path), there is still no
way to make the laptop running Herdr *be* the thing that plays. A user with
an Apple device has an obvious wish: pick this laptop in the AirPlay list,
hear the audio from its speakers, and see what is playing in the pane with
the same transport keys they already use.

## Solution

A second `MusicProvider` adapter, `airplay`, that turns the pane into an
AirPlay 1 (RAOP) **receiver**. When the TUI opens with `provider =
"airplay"`, the laptop advertises itself over mDNS as an AirPlay speaker
named `airplay_name` (default "Herdr Music"). A stock Apple sender streams
lossless ALAC to it; the adapter decodes to PCM and plays it through the
OS audio stack. The sender keeps doing DRM, accounts and licensing, so
there is no OAuth, no credential store and no audio-key problem.

The pane shows the sender's now-playing metadata and progress through the
same `Playback` type the TUI already renders. Transport keys send DACP
remote-control commands back to the sender, so `space`, `n`, `p` and the
seek keys work exactly as they do against Spotify. Everything above the
`MusicProvider` seam (`app::Controller`, `tui`, the status/footer rows) is
untouched apart from one provider-keyed idle hint.

The receiver is built on the `shairplay` crate (pure Rust, crates.io): RAOP
server, ALAC decode, mDNS advert, DMAP metadata and the `RemoteControl`
command path. It needs tokio; the adapter confines that runtime to one
thread it owns, so the UI thread and worker thread stay as they are.

## User Stories

Receiving:

1. As a Herdr user, I want my laptop to appear in the AirPlay list of my
   iPhone, iPad or Mac while the player pane is open, so that I can send
   music to it without installing anything else.
2. As a Herdr user, I want the audio to come out of the laptop's speakers
   or whatever output the OS has selected, so that the pane is a real
   player and not just a display.
3. As a Herdr user, I want the pane to say it is waiting for a sender until
   one connects, so that an empty pane is not mistaken for a failure.
4. As a Herdr user, I want the receiver gone from the AirPlay list the
   moment I quit the pane, so that no stray process keeps advertising.
5. As a Herdr user, I want a sender disconnecting to return the pane to the
   waiting state, so that stale metadata never lingers.

Now-playing and transport:

6. As a Herdr user, I want title, artist, album, elapsed and total time
   from the sender shown in the usual rows, so that the pane looks the same
   whichever provider is behind it.
7. As a Herdr user, I want `space`, `n`, `p`, `←`/`→` and clicks on the
   transport glyphs and scrub bar to control the sender, so that I never
   reach for the phone.
8. As a Herdr user, I want the volume slider on my phone to change the
   laptop's playback level, so that AirPlay behaves as it does with any
   speaker.
9. As a Herdr user, I want `d` to show the receiver and whether a sender is
   connected, so that the device picker stays meaningful.

Configuration and platforms:

10. As a Herdr user, I want to choose the provider with one key in
    `config.toml` (`provider = "airplay"`), so that switching between
    Spotify remote control and AirPlay receiving is a one-line change.
11. As a Herdr user, I want to rename the advertised receiver
    (`airplay_name`), so that several machines are distinguishable.
12. As a Herdr user, I want the receiver to work on Linux, macOS and
    Windows without installing development headers, so that `herdr plugin
    install` keeps compiling with only the Rust toolchain.
13. As a Herdr user on Linux, I want a clear status message when no
    PipeWire/PulseAudio/ALSA player binary is on PATH, so that I know what
    to install instead of hearing silence.
14. As a Herdr user, I want the README to state the shairplay license and
    what it means for the binary, so that attribution is honest.

## Implementation Decisions

### Module layout and the seam

- New module `airplay` with submodules: `receiver` (owns the tokio runtime
  thread and the shairplay server), `state` (the pure metadata/progress →
  `Playback` mapping and the event-driven state machine), `sink` (PCM
  output), and `mod.rs` holding `AirPlayProvider: MusicProvider` and
  `provider_from_config`.
- `AirPlayProvider` keeps an `Arc<Mutex<SessionState>>` shared with the
  runtime thread. All `MusicProvider` methods take `&self` and the trait is
  `Send`, so the worker thread in `tui::worker_loop` polls `playback()`
  every two seconds exactly as it does for Spotify, and reads never block
  on the network.
- `playback()` returns `Ok(None)` while no sender is connected, so
  `Controller::refresh` and `seek_relative` keep their existing semantics
  (`NoActiveDevice` on seek with nothing playing). With a session it
  returns the latest `Track` from DMAP metadata, `is_playing` from the
  sender's play state, and `progress` interpolated from the last RTP
  progress event plus elapsed wall time, so `status`, `seek_relative` and
  the TUI all see a current position. `device` is the sender's name when
  shairplay reports it, otherwise `None`.
- `play`/`pause`/`next`/`previous`/`seek` call the crate's `RemoteControl`
  (DACP) on the active session; with no session they return
  `NoActiveDevice`, whose existing display text already tells the user to
  start playback on a device.
- `devices()` returns one row: `{ id: "airplay", name: airplay_name,
  is_active: session connected }`. `transfer("airplay")` is `Ok(())`.
  Rationale: `Controller` opens the picker on `d` regardless, and an empty
  list draws a bordered empty box; one row tells the user the receiver's
  name and connection state at a glance. Alternative (empty list) is open
  question (b).
- `name()` returns `"airplay"`. `tui::accent_for` gains an `"airplay"` arm
  so the card has its own color; the idle hint in `draw` ("Nothing playing
  / Start a song on any device…") becomes provider-keyed like the accent,
  reading "Waiting for an AirPlay sender / Pick <airplay_name> in the
  AirPlay list" when the provider is `airplay`. This is the only TUI change.

### Runtime and lifecycle

- One dedicated OS thread owns a current-thread tokio runtime running the
  shairplay server; `Drop for AirPlayProvider` signals shutdown and joins
  the thread, so `tui::run` dropping the boxed provider after
  `worker.join()` unadvertises the receiver. No child process, no pidfile,
  no reaper: `specs/local-playback.md` spent a third of its decisions on
  orphan prevention for an engine child, and that machinery is exactly
  what in-process ownership makes unnecessary.
- tokio is permitted inside `airplay::receiver` only. Nothing above the
  seam gains an async signature; `std::sync::mpsc` between UI and worker
  stays. The `initial.md` dependency policy is amended to say so.
- Sender events (connected, metadata, progress, play state, volume,
  disconnected) are applied straight to the shared `SessionState` by the
  shairplay callbacks, and commands leave through a small
  `SenderControl` trait wrapping shairplay's `RemoteControl`. That pair is
  the adapter's test seam, mirroring the base-URL seam in `spotify`.

### Audio output

- Decoded PCM is s16le, 44.1 kHz, stereo. Volume from the sender arrives
  in dB (−30…0, −144 = mute); `airplay::sink` applies it as linear gain
  (`10^(dB/20)`, mute at −144) to the samples before writing, a pure
  function.
- Linux: pipe PCM to the first available of `pw-play`, `paplay`, `aplay`,
  found on PATH, with raw-format arguments matching the stream. The argv
  builder is a pure function of the chosen binary. This is the same design
  the withdrawn spec used and it needs no ALSA headers. If none is found,
  the adapter still accepts the session but sets a status the TUI shows:
  "no audio player found — install pipewire-utils, pulseaudio-utils or
  alsa-utils". `cpal` on Linux was rejected because it requires
  `libasound2-dev` at build time.
- macOS and Windows: `cpal` (CoreAudio / WASAPI, frameworks ship with the
  OS), gated with `#[cfg]` so Linux never compiles it.

### Configuration and dispatch

- `config::AppConfig` gains `provider: ProviderKind` (default `Spotify`)
  and `airplay_name: String` (default "Herdr Music"), read from
  `config.toml` with the same hand-rolled line parser as
  `spotify_client_id`, generalized to look up a key. No `toml` crate.
  An unknown `provider` value is an error at load, reported in the TUI
  status line and as a non-zero `ctl` exit, not silently defaulted.
- `main::dispatch` for `Tui` selects the adapter from `cfg.provider`
  through a `provider::from_config` that returns `Box<dyn MusicProvider>`.
  `ctl` and `status` today build a fresh `SpotifyProvider` in a separate
  process; an AirPlay session lives inside the TUI process, so without IPC
  those commands cannot reach the sender. For this spec they return a
  clear error under `provider = "airplay"` ("the AirPlay receiver runs
  inside the player pane"); an IPC path is open question (d).
- `auth` subcommands stay Spotify-only and are unaffected.

### Manifest and docs

- `herdr-plugin.toml` is unchanged in shape; `platforms` already lists all
  three OSes. The description gains "or an AirPlay receiver".
- README: config keys, the Linux player-binary note, the firewall prompt
  users will see on macOS/Windows (RTSP TCP plus RTP/control/timing UDP
  ports), and the license note below.

### Risks

- **License.** `shairplay` is LGPL-3.0; Rust links it statically into an
  Apache-2.0 binary. LGPL §4(d)(0) allows this when the user can relink
  against a modified library, which the plugin satisfies by construction:
  it is distributed as source and `[[build]]` runs `cargo build --release`
  on the user's machine, so replacing the crate and rebuilding *is* the
  relink. Decision: keep it, state the LGPL-3.0 dependency and this
  reasoning in the README and a `NOTICE` file. Fallback if the owner
  disagrees: put the adapter behind a Cargo feature so a binary built
  without it carries no LGPL code.
- **Binary size.** Today's release build is about 3 MB. tokio, the RAOP
  crypto (RSA/AES), the ALAC decoder, mDNS and (mac/win) cpal are expected
  to take it to roughly 10–15 MB even with LTO and strip. The first task
  measures it and records the number here. Whether that cost is always-on
  or behind an `airplay` Cargo feature is open question (a); the
  recommendation is always-on for one build path and one manifest.
- **Crate maturity.** shairplay 0.13 is young; the `SenderEvents` /
  `SenderControl` seam exists so a crate swap stays inside
  `airplay::receiver`.
- **No remote control on macOS.** shairplay's `discover_dacp_port`
  returns `None` on macOS (its Bonjour backend has no synchronous
  browse+resolve), so it falls back to `sender_ip:3689` — a port iOS
  does not listen on — and transport verbs fail. Linux/Windows do the
  real mDNS lookup. Documented in the README; no in-tree workaround —
  transport keys show the error until upstream fixes port discovery.

## Testing Decisions

Tests cross the `MusicProvider` seam or exercise pure functions; nothing
asserts tokio internals or shairplay call order. No test opens a network
port or needs an Apple device.

- **State machine** (`airplay::state`): feed a scripted sequence of
  `SenderEvents` into the provider and assert `playback()` through the
  trait: no session → `Ok(None)`; metadata + progress → the expected
  `Playback`; play state flips `is_playing`; progress advances with
  injected elapsed time only while playing; disconnect returns to
  `Ok(None)`; a seek command from the sender side updates progress.
- **Remote control**: a recording fake `SenderControl` behind the provider;
  assert each trait verb (`play`, `pause`, `next`, `previous`, `seek`)
  produces the matching DACP command, and that every verb returns
  `NoActiveDevice` with no session. `play_pause` and `seek_relative` are
  already covered on `FakeProvider` and are not re-tested here.
- **Devices**: `devices()` yields the single receiver row with `is_active`
  tracking the session; `transfer("airplay")` is `Ok`.
- **Sink**: pure tests for dB → linear gain (0 dB → 1.0, −30 dB ≈ 0.0316,
  −144 → 0.0) and for the Linux argv builder for each of `pw-play`,
  `paplay`, `aplay`; the selection order given a fake "which" list; the
  no-player status text. The cpal path and real audio are manual.
- **Config**: `AppConfig` parsing of `provider` (absent → Spotify,
  `"airplay"`, unknown → error) and `airplay_name` default, as unit tests
  next to the existing `parse_client_id` tests.
- **Dispatch**: `provider::from_config` picks the adapter kind from
  `AppConfig` (asserted on `name()`), and `ctl`/`status` under
  `provider = "airplay"` fail with the explanatory message and exit 1.
- **TUI**: with `provider_name = "airplay"` and an empty snapshot the
  rendered buffer contains the waiting hint and not "Nothing playing";
  with a snapshot it renders the same rows as the Spotify case (reuse
  `draws_track_details`). No new clickable regions, so `regions()` tests
  are unchanged.
- **Manual**: iPhone → laptop on each OS: receiver appears in the list,
  audio plays, `space`/`n`/`p`/seek and the phone's volume slider work,
  `q` removes it from the list within a few seconds.

## Out of Scope

- AirPlay 2 (the crate's `ap2` feature stays off; a later spec).
- Sender mode: controlling AirPlay or Cast speakers from the pane.
- Google Cast receiving (Google-signed device certificates make it
  impossible), Android senders, screen or video mirroring.
- Multiple simultaneous senders, multiroom sync, output-device selection.
- An OS media-session provider (MPRIS, SMTC, NowPlaying).
- `ctl`/`status` reaching a running receiver from another process (see
  open question d).
- Album art (DMAP delivers it; `Track` has no field, and the TUI has no
  row).

## Further Notes

- Platform matrix: receiver on Linux, macOS, Windows; senders are Apple
  devices only. Android users stay on `provider = "spotify"`.
- shairplay's mDNS uses `mdns-sd` on Linux/Windows and `astro-dnssd` on
  macOS, so no Avahi daemon or D-Bus is required.
- Rejected libraries: `openairplay1` (Avahi-only discovery, no remote
  control), `rust-airplay` (no decoder), shairport-sync (C, heavy native
  deps, no Windows).
- Development loop is unchanged: `cargo build --release` → `herdr plugin
  link` → open the pane, with `provider = "airplay"` in the plugin config
  dir's `config.toml`.

### Decisions (answered 2026-11-06)

- **Always-on** — no Cargo feature; one binary, `provider` config picks the adapter.
- **`devices()` returns the receiver row** — "Herdr Music (AirPlay)", active when a sender is connected.
- **`provider` is a config.toml key** — default `spotify`.
- **`ctl`/`status` under airplay prints a clear error** — no IPC in this spec.
