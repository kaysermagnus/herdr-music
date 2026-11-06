# herdr-music

A [Herdr](https://herdr.dev) plugin: TUI music controller that lives in a
pane next to your work. Written in Rust around a provider interface — the
first adapter is **Spotify**.

- Now playing: track, artists, album, progress bar, device
- Controls: play / pause / stop, next / previous, seek ±10s
- Headless actions for every control, bindable to Herdr keys
- Small footprint: ~3 MB static binary, no async runtime, minimal deps

## Prerequisites

- **Rust** (stable) — for building
- **Spotify Premium** — the Web API requires it for playback control
- A **Spotify Developer app**: create one at
  <https://developer.spotify.com/dashboard> and add
  `http://127.0.0.1:8888/callback` as a Redirect URI

## Configure

Give the plugin your app's client id — either as an env var:

```sh
export HERDR_MUSIC_SPOTIFY_CLIENT_ID=your_client_id
```

or in `config.toml` inside the plugin config dir
(`herdr plugin config-dir cesar.music` prints it):

```toml
spotify_client_id = "your_client_id"
```

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

The `[[build]]` step in `herdr-plugin.toml` runs `cargo build --release`.

## Use

```sh
herdr plugin pane open --plugin cesar.music --entrypoint player  # split pane
herdr plugin action invoke cesar.music.spotify-login             # connect account
# If the browser doesn't open, find the authorize URL in:
#   herdr plugin log list --plugin cesar.music
herdr plugin action invoke cesar.music.play-pause                # headless control
```

Or run the binary directly (`HERDR_PLUGIN_*` dirs fall back to platform
dirs): `herdr-music tui`, `herdr-music ctl next`, `herdr-music status`,
`herdr-music auth login`.

### TUI keys

| key | action |
| --- | --- |
| `space` | play / pause |
| `s` | stop (pause — Spotify has no stop) |
| `n` / `p` | next / previous track |
| `→` `l` / `←` `h` | forward / rewind 10s |
| `r` | refresh now playing |
| `q` | quit |

### Keybindings in Herdr

Add to your Herdr `config.toml`:

```toml
[[keys.command]]
key = "prefix+m"
type = "plugin_action"
command = "cesar.music.play-pause"
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
cargo test            # 46 tests: unit + stub-server integration
cargo build --release
```

Spec and task breakdown: `SPEC.md`, `tasks.md`.
