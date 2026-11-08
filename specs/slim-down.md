# Spec: slim-down — remove local playback, keep the device picker

## Problem Statement

Between `22144f1` and `1801327` the plugin grew a "local playback" engine: a
librespot child process managed by `engine`, configured through
`spotify::local`, surfaced in the TUI as a `(this laptop)` picker row and a
status alert. Live verification showed it is dead: Spotify denies audio
decryption keys ("error audio key 0 1") to every third-party credential path
for this account class (librespot #1649; PR #1847 fixes sign-in, not keys).
It ships disabled by default, yet it still costs ~830 lines, a `Mutex<Phase>`
state machine inside the Spotify adapter, a `streaming` scope, three
librespot build steps in the manifest, and a README full of prerequisites
for a feature that cannot work. The plugin was specified as a small,
service-agnostic remote control; the engine contradicts that.

## Solution

One forward commit on `main` that deletes the engine and everything that
exists only to serve it, while keeping the device picker, which is a generic
remote-control concept (Connect/AirPlay/Cast) and resolves Spotify's
"no active device" dead-end from inside the pane. History is not rewritten;
`specs/local-playback.md` stays with a short "Withdrawn" note at the top.

## User Stories

1. As a Herdr user, I want the plugin to build with `cargo build --release`
   alone, so that installing it never compiles or downloads librespot.
2. As a Herdr user, I want the pane never to show a local-playback alert or a
   `(this laptop)` row, so that it does not advertise a capability Spotify
   denies.
3. As a Herdr user, I want `d` to still list my Connect devices and Enter to
   transfer playback, so that I can wake a device without leaving the pane.
4. As a Herdr user, I want a stale `local_playback = true` in my config to be
   ignored silently, so that the upgrade needs no migration.
5. As a maintainer, I want the Spotify adapter to hold no engine state, so
   that a second provider has a small, honest trait to implement.

## Implementation Decisions

Owner decisions, final:

- Remove the local-playback engine entirely.
- Keep the device picker: `Device`, `devices()`/`transfer()` on the trait,
  the `d` key, picker rows/click/draw, the Spotify `GET /me/player/devices`
  and `PUT /me/player` implementation and their stub-server tests.
- Provider expansion is out of scope (see below).
- Plain edits and deletions in one commit; no history rewrite.
  `specs/local-playback.md` gets a 2–3 line "Withdrawn" note (date, reason =
  Spotify audio-key denial, pointer to git history for the implementation);
  its "post-spec upstream change" / "final verdict" bullets are deleted.
- Scopes: drop `streaming` from the requested list in `spotify::auth`. Keep
  the `scopes` field on `StoredTokens` with its serde default; persisted
  token files remain readable and `has_scope` stays available.

Removal map:

- Delete `src/engine.rs`, `src/spotify/local.rs`, `tests/engine.rs`.
- `lib`: drop `pub mod engine;`.
- `provider`: drop `LocalStatus`, `ProviderError::LocalPlayback`,
  `start_local`/`stop_local`/`local_status` (with defaults), and in
  `FakeProvider` `set_local_status`, those three impls and their recorded
  calls. Keep `Device`, `devices()`, `transfer()`, `set_devices`, and the
  `transfer:<id>` call record.
- `spotify::mod`: drop `pub mod local;`, `READY_TIMEOUT`, `OAUTH_TIMEOUT`,
  `Phase`, `EngineInvocation`, the `local` and `engine_invocation` fields,
  `with_engine_invocation()`, and the three local trait impls. Keep
  `devices()` and `transfer()`.
- `app`: drop `Command::StartLocal`, `PickerAction::StartLocal` (collapse
  `PickerAction` to transfer, or map rows straight to a device id), the
  synthetic local rows in `picker_rows()`, `Snapshot.local_name` /
  `local_status` / `local_alert`, controller fields `local_name`,
  `local_status`, `alert_dismissed`, `transfer_attempted`, `with_local()`,
  the auto-transfer logic and the alert composition. Keep
  `Command::Devices`, `PickerMove`, `PickerSelect`, `SelectDevice`, and
  `picker_open`/`picker_index`/`devices` in `Snapshot`.
- `tui`: `run(provider: Box<dyn MusicProvider>)`; the worker no longer
  sends `StartLocal`; drop the `LocalStatus` import, the local-alert branch
  of the status line (reverts to playback/error/hint) and the
  `(this laptop…)` labels. Keep the picker footer, keys (`d`, j/k/arrows,
  Enter, Esc), picker rows in `regions()`, click mapping, active marker.
- `config`: drop `local_playback`, `local_device_name`,
  `DEFAULT_LOCAL_DEVICE_NAME`, `parse_local()`, `local_device()`.
- `main`: `tui::run(Box::new(provider))`.
- `herdr-plugin.toml`: drop the three librespot `[[build]]` entries and
  their comment; keep `cargo build --release` and
  `platforms = ["linux", "macos", "windows"]`.
- `README.md`: drop the local-playback bullet, the Rust ≥1.85 / MSVC /
  Linux-audio prerequisites, the `local_playback`/`local_device_name` config
  block and upstream-block paragraph, the librespot build paragraph, the
  "Local playback and devices" section (replace with 2–3 lines on the `d`
  picker), the librespot `## Credits`, and the local-playback spec pointer.
  Keep the `d` row in the key table.

Risk: users with `local_playback = true` in `config.toml` get an ignored
key; the parser already ignores unknown keys, so no migration is needed.

## Testing Decisions

This is a removal, so the work is deleting tests that only observed the
engine and confirming the surviving ones still pin the kept behavior.

- `FakeProvider` seam (`app` tests): remove `local_controller`,
  `start_local_records_call_and_status_lands_on_snapshot`,
  `start_local_error_becomes_a_status_not_a_panic`, `auto_transfer_*`,
  `failure_alert_hides_after_picker_opens`, `silent_disabled_never_alerts`;
  trim `picker_rows_label_local_and_mark_active` to the active marker only.
  Keep `devices_fills_the_snapshot_list`,
  `select_device_transfers_then_refreshes`, and picker move/select tests.
- Stub HTTP server seam (`tests/spotify_client.rs`): remove `fake_engine`,
  `fake_invocation`, `provider_with_engine`, `scoped_tokens` if unused,
  unused `DEVICES_*` consts, both `start_local_*` tests and the
  `Invocation`/`LocalStatus` imports. Keep `devices_maps_the_device_list`,
  `transfer_puts_me_player_with_a_json_body`,
  `transfer_404_is_no_active_device`, and the `Content-Length: 0` assertion.
- `regions()` seam (`tui` tests): remove the engine status-line and local
  picker-label tests; keep picker geometry, draw/click agreement and key
  tests so the `d` picker stays covered where its click maps.
- `config` tests: remove `local_playback_keys_parse_with_defaults`.
- Verification: `cargo fmt && cargo test` green (roughly 85–90 tests, down
  from 108); `cargo build --release`;
  `rg -i 'librespot|local_playback|LocalStatus|start_local|engine' src tests README.md herdr-plugin.toml`
  returns nothing (the withdrawn note in `specs/` is the only hit
  repo-wide); CI matrix green on linux/macos/windows after push; manual:
  open the pane, `d` lists Connect devices, Enter transfers, `q` leaves no
  child process.

## Out of Scope

- Provider expansion: other services, a `provider` config key, OS
  media-session control (MPRIS/SMTC), a subprocess plugin protocol. A later
  spec once a second provider is actually wanted. Apple Music and YouTube
  Music have no public remote-control API, so per-service adapters cannot
  deliver them; MPD/Jellyfin/Plex-style services and OS media sessions are
  the realistic candidates.
- Any replacement for local playback, including waiting on librespot fixes.
- Rewriting history to make the engine commits disappear.
- Changing picker behavior or keys beyond removing the local row.

## Further Notes

- The engine implementation remains reachable in git history between
  `22144f1` and `1801327` should Spotify ever grant audio keys again.
- After the change the only Spotify scopes requested are the three from
  `specs/initial.md`; existing token files that carry `streaming` keep
  working because the adapter never checks for it.
