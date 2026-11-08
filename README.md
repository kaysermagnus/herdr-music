# herdr-music

A [Herdr](https://herdr.dev) plugin: TUI music controller that lives in a
pane next to your work. Written in Rust around a provider interface — the
first adapter is **Spotify**, the second an **AirPlay receiver**.

- Now playing: track, artists, album, progress bar, device
- Controls: play / pause / stop, next / previous, seek ±10s
- Device picker: `d` lists your Spotify Connect devices, Enter moves
  playback there
- AirPlay: stream audio from an iPhone, iPad or Mac to the pane
- Headless actions for every control, bindable to Herdr keys
- Small footprint: ~3.5 MB static binary, minimal deps

## Prerequisites

- **Rust** (stable) — for building
- **Spotify Premium** — required by the Web API
- A **Spotify Developer app**: create one at
  <https://developer.spotify.com/dashboard> and add
  `http://127.0.0.1:8888/callback` as a Redirect URI

## Configure

Give the plugin your app's client id — either as an env var:

```sh
export HERDR_MUSIC_SPOTIFY_CLIENT_ID=your_client_id
```

or in `config.toml` inside the plugin config dir
(`herdr plugin config-dir herdr-music` prints it):

```toml
spotify_client_id = "your_client_id"
```

`config.toml` keys:

| key | default | meaning |
| --- | --- | --- |
| `spotify_client_id` | — | your Spotify app's client id |
| `provider` | `"spotify"` | adapter: `"spotify"` or `"airplay"` |
| `airplay_name` | `"Herdr Music"` | name advertised in the AirPlay list |

## Install

From source during development:

```sh
cargo build --release
herdr plugin link /path/to/herdr-music
```

From GitHub (once pushed):

```sh
herdr plugin install kaysermagnus/herdr-music
```

## Use

```sh
herdr plugin pane open --plugin herdr-music --entrypoint player  # split pane
herdr plugin action invoke herdr-music.spotify-login             # connect account
# If the browser doesn't open, find the authorize URL in:
#   herdr plugin log list --plugin herdr-music
herdr plugin action invoke herdr-music.play-pause                # headless control
```

Or run the binary directly (`HERDR_PLUGIN_*` dirs fall back to platform
dirs): `herdr-music tui`, `herdr-music ctl next`, `herdr-music status`,
`herdr-music auth login`.

### TUI

```
┌ spotify ─────────────────────────────────────────┐
│                                                  │
│  ▂▅▃▇  Midnight City                             │
│        M83                                       │
│        Hurry Up, We're Dreaming                  │
│                                                  │
│  1:23  ━━━━━━━━━━●───────────────────────  4:03  │
│                                                  │
│                  ◀◀     ❚❚     ▶▶                │
│                                                  │
│  on Cesar's Mac                                  │
│  space play/pause   n/p skip   ←/→ 10s   q quit  │
└──────────────────────────────────────────────────┘
```

The level meter moves while playing and rests while paused. The center
glyph is the action `space` will take.

### Devices

Press `d` to open the device picker: every Spotify Connect device on the
account, with the active one marked. Enter (or click) transfers playback
to the highlighted device — handy when nothing is playing anywhere.

Mouse: click `◀◀` / `▶` / `▶▶` to skip back, play/pause, or skip ahead.
Click anywhere on the bar to jump to that spot. The player captures the
mouse while open, so hold your terminal's selection modifier (often
`shift`, or `option` on macOS) to select text in its pane.

| key | action |
| --- | --- |
| `space` | play / pause |
| `s` | stop (pause — Spotify has no stop) |
| `n` / `p` | next / previous track |
| `→` `l` / `←` `h` | forward / rewind 10s |
| `r` | refresh now playing |
| `d` | device picker (`↑/↓`/`j/k` move, `enter` select, `d`/`esc` close) |
| `q` / `esc` | quit |

### Keybindings in Herdr

Add to your Herdr `config.toml`:

```toml
[[keys.command]]
key = "prefix+m"
type = "plugin_action"
command = "herdr-music.play-pause"
description = "music play/pause"
```

## AirPlay

With `provider = "airplay"` the pane becomes an AirPlay 1 (RAOP)
**receiver** instead of a Spotify remote: this laptop advertises itself
as a speaker named `airplay_name` (default "Herdr Music") while the pane
is open.

- Senders: Apple devices only — iPhone, iPad or Mac. Pick the laptop in
  the AirPlay/audio output list; there is nothing to install and no
  account to connect.
- The pane shows the sender's now-playing metadata and the transport
  keys (`space`, `n`, `p`) drive the sender back over DACP. The sender's
  volume slider changes the laptop's playback level. Seeking is not
  supported over AirPlay — the `←`/`→` keys will tell you so.
- Remote control currently works on Linux and Windows only. shairplay
  cannot discover the sender's DACP port on macOS yet, so the transport
  keys show an error there until upstream fixes port discovery;
  receiving, metadata and volume work on all three platforms.
- Setup: set `provider = "airplay"` in `config.toml`, open the pane, pick
  the laptop on your phone. The pane shows "Waiting for an AirPlay
  sender" until something connects, and quitting the pane unadvertises
  the receiver.
- `d` shows the receiver row — active while a sender is connected.
- `ctl`, `status` and `auth` run in a separate process and cannot reach a
  session inside the pane, so under `provider = "airplay"` they exit with
  an explanatory error.
- On macOS/Windows the OS may ask to allow incoming connections the first
  time (RTSP TCP plus RTP/control/timing UDP ports) — accept it once.
- On Linux, audio plays through the first of `pw-play`, `paplay` or
  `aplay` on PATH (`pipewire-utils`, `pulseaudio-utils` or `alsa-utils`).
  With none installed the pane keeps accepting sessions and tells you
  what to install instead of staying silent.

## Architecture

```
            ┌──────────────┐
            │  MusicProvider│  ← the seam (provider.rs)
            │    trait      │   playback / play / pause / next /
            └──────┬───────┘   previous / seek / play_pause /
        ┌──────────┴──┐        seek_relative
        ▼             ▼
   SpotifyProvider   AirPlayProvider
   oauth+web api      RAOP receiver (shairplay)
```

- TUI, `ctl`, and `status` only ever see `MusicProvider`; tests exercise
  everything above the seam with an in-memory `FakeProvider`.
- The Spotify adapter is tested through the same trait against a stub
  HTTP server (its endpoints are injectable); the AirPlay adapter keeps
  its shairplay events behind a `SenderEvent`/`SenderControl` seam.
- Two threads: UI (crossterm events, ~100 ms tick) + worker (commands and a
  2 s playback poll) over `std::sync::mpsc`. The AirPlay receiver owns a
  third thread for its tokio runtime, confined to `src/airplay/receiver.rs`.

## Development

```sh
cargo test            # unit + stub-server integration tests
cargo build --release
```

Specs live in `specs/`: the original plugin spec is `specs/initial.md`,
with its task breakdown in `specs/initial-tasks.md`.

## Credits

- [ratatui](https://ratatui.rs) + crossterm — the TUI
- [shairplay](https://github.com/metaneutrons/shairplay-rust) — the AirPlay
  receiver, **LGPL-3.0**. Rust links crates statically, which LGPL §4(d)(0)
  allows when the user can relink a modified library; this plugin is
  distributed as source and `cargo build --release` runs on your machine,
  so replacing the crate and rebuilding is the relink. See `NOTICE`.
