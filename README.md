# herdr-music

A [Herdr](https://herdr.dev) plugin: TUI music controller that lives in a
pane next to your work. Written in Rust around a provider interface — the
first adapter is **Spotify**.

- Now playing: track, artists, album, progress bar, device
- Controls: play / pause / stop, next / previous, seek ±10s
- **Local playback**: the laptop itself becomes a Spotify Connect device
  while the player pane is open (press `d` to pick where music plays)
- Headless actions for every control, bindable to Herdr keys
- Small footprint: ~3 MB static binary, no async runtime, minimal deps

## Prerequisites

- **Rust** (stable, ≥ 1.85 — required by the playback engine) — for
  building. On Windows this means the MSVC toolchain.
- **Spotify Premium** — required by both the Web API and local playback
- A **Spotify Developer app**: create one at
  <https://developer.spotify.com/dashboard> and add
  `http://127.0.0.1:8888/callback` as a Redirect URI
- On **Linux**, one audio player: `pw-play` (PipeWire), `paplay`
  (PulseAudio), or `aplay` (ALSA) — almost always already installed

## Configure

Give the plugin your app's client id — either as an env var:

```sh
export HERDR_MUSIC_SPOTIFY_CLIENT_ID=your_client_id
```

or in `config.toml` inside the plugin config dir
(`herdr plugin config-dir herdr-music` prints it):

```toml
spotify_client_id = "your_client_id"

# Optional: local playback (both have these defaults)
local_playback = true          # set false to stay a pure remote control
local_device_name = "Herdr Music"
```

Note: accounts connected before local playback shipped need one more
`herdr-music auth login` to grant the `streaming` scope — the device
picker tells you when that's the case.

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

The `[[build]]` steps in `herdr-plugin.toml` run `cargo build --release`,
then compile [librespot](https://github.com/librespot-org/librespot)
(MIT) — the Spotify Connect engine used for local playback — pinned to
version 0.8.0. The engine build takes a few minutes on a clean machine;
re-installs are fast when the pinned version is already present.

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

### Local playback and devices

While the pane is open, a bundled librespot engine registers this laptop
as a Connect device. Press `d` to open the device picker — every Connect
device on the account plus this laptop — and Enter (or click) to move
playback there. The engine starts and stops with the pane; nothing keeps
playing or lingers after you quit. On Linux it pipes audio to `pw-play`,
`paplay`, or `aplay`; on macOS and Windows it uses the OS audio stack.

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

## Architecture

```
            ┌──────────────┐
            │  MusicProvider│  ← the seam (provider.rs)
            │    trait      │   playback / play / pause / next /
            └──────┬───────┘   previous / seek / play_pause /
        ┌──────────┴──┐        seek_relative
        ▼             ▼
   SpotifyProvider   (future adapters:
   oauth+web api      apple music, mpd, …)
```

- TUI, `ctl`, and `status` only ever see `MusicProvider`; tests exercise
  everything above the seam with an in-memory `FakeProvider`.
- The adapter is tested through the same trait against a stub HTTP server
  (its endpoints are injectable).
- Two threads: UI (crossterm events, ~100 ms tick) + worker (commands and a
  2 s playback poll) over `std::sync::mpsc`. No tokio.

## Development

```sh
cargo test            # unit + stub-server integration tests
cargo build --release
```

Specs live in `specs/`: the original plugin spec is `specs/initial.md`,
with its task breakdown in `specs/initial-tasks.md`, and local playback
in `specs/local-playback.md`.

## Credits

Local playback uses [librespot](https://github.com/librespot-org/librespot),
built from crates.io at install time under the MIT license. It is
compiled on your machine — no binary is bundled or redistributed.
