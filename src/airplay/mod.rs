//! AirPlay receiver adapter: the pane becomes an AirPlay 1 (RAOP) speaker
//! named `airplay_name`. The sender streams ALAC; shairplay decodes to
//! PCM for [`sink`]; metadata/progress flow through [`state`] into the
//! shared `Playback` type; transport verbs go back over DACP via
//! [`SenderControl`].

mod receiver;
/// PCM output — public so its platform-agnostic parts (gain, player
/// argv) are testable on any target.
pub mod sink;
/// The sender-event → `Playback` state machine — public for its tests.
pub mod state;

use crate::config::AppConfig;
use crate::provider::{Device, MusicProvider, Playback, ProviderError};
use receiver::Receiver;
use state::SessionState;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The device id the picker row advertises for this receiver.
const RECEIVER_DEVICE_ID: &str = "airplay";

/// DACP remote control of the active sender — the adapter's command seam
/// (mirrors the base-URL seam in `spotify`).
pub trait SenderControl: Send + Sync {
    fn play(&self) -> Result<(), ProviderError>;
    fn pause(&self) -> Result<(), ProviderError>;
    fn next(&self) -> Result<(), ProviderError>;
    fn previous(&self) -> Result<(), ProviderError>;
    fn seek(&self, position: Duration) -> Result<(), ProviderError>;
}

/// shairplay's `RemoteControl` (AP1 DACP) adapted to [`SenderControl`].
///
/// Note on coverage: shairplay's `RemoteCommand` has no seek command —
/// `Play`/`Pause` both reach the sender's `playpause`, and DACP's
/// `setproperty?dacp.playingtime` is not exposed by the crate. `seek`
/// therefore reports it is unsupported rather than doing nothing.
struct DacpSenderControl {
    remote: Arc<dyn shairplay::RemoteControl>,
}

impl DacpSenderControl {
    fn new(remote: Arc<dyn shairplay::RemoteControl>) -> Self {
        Self { remote }
    }

    fn send(&self, command: shairplay::RemoteCommand) -> Result<(), ProviderError> {
        self.remote
            .send_command(command)
            .map_err(|e| ProviderError::Api(format!("airplay control: {e}")))
    }
}

impl SenderControl for DacpSenderControl {
    fn play(&self) -> Result<(), ProviderError> {
        self.send(shairplay::RemoteCommand::Play)
    }
    fn pause(&self) -> Result<(), ProviderError> {
        self.send(shairplay::RemoteCommand::Pause)
    }
    fn next(&self) -> Result<(), ProviderError> {
        self.send(shairplay::RemoteCommand::NextTrack)
    }
    fn previous(&self) -> Result<(), ProviderError> {
        self.send(shairplay::RemoteCommand::PreviousTrack)
    }
    fn seek(&self, _position: Duration) -> Result<(), ProviderError> {
        Err(ProviderError::Api(
            "seeking is not supported over AirPlay".into(),
        ))
    }
}

/// `MusicProvider` for the receiver: `playback()` answers from shared
/// session state (never the network), transport verbs reach the sender's
/// DACP service through [`SenderControl`].
pub struct AirPlayProvider {
    airplay_name: String,
    state: Arc<Mutex<SessionState>>,
    /// `None` in tests — the receiver only runs inside the real pane.
    _receiver: Option<Receiver>,
}

impl AirPlayProvider {
    /// Advertise `airplay_name` and start receiving.
    pub fn new(airplay_name: String) -> Result<Self, ProviderError> {
        let state = Arc::new(Mutex::new(SessionState::new()));

        #[cfg(target_os = "linux")]
        let player = match sink::detect_player() {
            some @ Some(_) => some,
            None => {
                state
                    .lock()
                    .unwrap()
                    .set_sink_error(sink::NO_PLAYER_MSG.to_string());
                None
            }
        };
        #[cfg(not(target_os = "linux"))]
        let player = ();

        let receiver = Receiver::spawn(airplay_name.clone(), state.clone(), player)
            .map_err(|e| ProviderError::Api(format!("airplay receiver: {e}")))?;
        Ok(Self::with_parts(airplay_name, state, Some(receiver)))
    }

    fn with_parts(
        airplay_name: String,
        state: Arc<Mutex<SessionState>>,
        receiver: Option<Receiver>,
    ) -> Self {
        Self {
            airplay_name,
            state,
            _receiver: receiver,
        }
    }

    /// A provider with no receiver — tests drive `state` directly, the
    /// same way the receiver's callbacks do.
    #[cfg(test)]
    fn testing() -> (Self, Arc<Mutex<SessionState>>) {
        let state = Arc::new(Mutex::new(SessionState::new()));
        let provider = Self::with_parts("Herdr Music".into(), state.clone(), None);
        (provider, state)
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, SessionState> {
        self.state.lock().unwrap()
    }
}

impl MusicProvider for AirPlayProvider {
    fn name(&self) -> &'static str {
        "airplay"
    }

    fn receiver_name(&self) -> Option<String> {
        Some(self.airplay_name.clone())
    }

    fn playback(&self) -> Result<Option<Playback>, ProviderError> {
        self.locked().playback(Instant::now())
    }

    // Every verb takes the DACP handle out of state, then releases the
    // lock before calling it: a DACP request can block for seconds and
    // the audio path takes this lock for every PCM chunk.
    fn play(&self) -> Result<(), ProviderError> {
        let state = self.locked();
        let control = state.control()?;
        // shairplay maps Play and Pause to the same DACP `playpause`
        // toggle — sending it while already playing would pause. Only
        // toggle when the session is not in the asked-for state.
        if state.is_playing() {
            return Ok(());
        }
        drop(state);
        control.play()
    }
    fn pause(&self) -> Result<(), ProviderError> {
        let state = self.locked();
        let control = state.control()?;
        // Same toggle gate: `s`/`stop` on a paused session must not
        // restart it.
        if !state.is_playing() {
            return Ok(());
        }
        drop(state);
        control.pause()
    }
    fn next(&self) -> Result<(), ProviderError> {
        let control = self.locked().control()?;
        control.next()
    }
    fn previous(&self) -> Result<(), ProviderError> {
        let control = self.locked().control()?;
        control.previous()
    }
    fn seek(&self, position: Duration) -> Result<(), ProviderError> {
        let control = self.locked().control()?;
        control.seek(position)
    }

    /// One row: this receiver, active while a sender session is live.
    fn devices(&self) -> Result<Vec<Device>, ProviderError> {
        let connected = self.locked().connected();
        Ok(vec![Device {
            id: RECEIVER_DEVICE_ID.into(),
            name: format!("{} (AirPlay)", self.airplay_name),
            is_active: connected,
        }])
    }

    /// The only row is this receiver — selecting it is already done.
    fn transfer(&self, device_id: &str) -> Result<(), ProviderError> {
        if device_id == RECEIVER_DEVICE_ID {
            Ok(())
        } else {
            Err(ProviderError::Api(format!("unknown device `{device_id}`")))
        }
    }
}

/// Build the adapter from loaded config.
pub fn provider_from_config(cfg: &AppConfig) -> Result<AirPlayProvider, ProviderError> {
    AirPlayProvider::new(cfg.airplay_name.clone())
}

#[cfg(test)]
mod tests {
    use super::state::SenderEvent;
    use super::*;
    use std::sync::Mutex;

    /// `SenderControl` recording the verbs the provider asks for.
    struct RecordingControl {
        calls: Mutex<Vec<String>>,
    }

    impl RecordingControl {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
            })
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl SenderControl for RecordingControl {
        fn play(&self) -> Result<(), ProviderError> {
            self.calls.lock().unwrap().push("play".into());
            Ok(())
        }
        fn pause(&self) -> Result<(), ProviderError> {
            self.calls.lock().unwrap().push("pause".into());
            Ok(())
        }
        fn next(&self) -> Result<(), ProviderError> {
            self.calls.lock().unwrap().push("next".into());
            Ok(())
        }
        fn previous(&self) -> Result<(), ProviderError> {
            self.calls.lock().unwrap().push("previous".into());
            Ok(())
        }
        fn seek(&self, position: Duration) -> Result<(), ProviderError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("seek:{}", position.as_secs()));
            Ok(())
        }
    }

    /// shairplay `RemoteControl` recording `RemoteCommand`s — the wire end
    /// of `DacpSenderControl`.
    struct FakeRemote {
        sent: Mutex<Vec<shairplay::RemoteCommand>>,
    }

    impl shairplay::RemoteControl for FakeRemote {
        fn send_command(
            &self,
            cmd: shairplay::RemoteCommand,
        ) -> Result<(), shairplay::ShairplayError> {
            self.sent.lock().unwrap().push(cmd);
            Ok(())
        }
        fn available_commands(&self) -> Vec<shairplay::RemoteCommand> {
            Vec::new()
        }
    }

    /// Apply an event the way the receiver's callbacks do: straight to
    /// shared state, stamped now.
    fn send(state: &Arc<Mutex<SessionState>>, event: SenderEvent) {
        state.lock().unwrap().apply(event, Instant::now());
    }

    fn connect(state: &Arc<Mutex<SessionState>>) {
        send(
            state,
            SenderEvent::Connected {
                name: Some("10.0.0.4".into()),
            },
        );
    }

    fn progress(state: &Arc<Mutex<SessionState>>) {
        send(
            state,
            SenderEvent::Progress {
                start: 0,
                current: 60 * 44_100,
                end: 200 * 44_100,
            },
        );
    }

    #[test]
    fn name_and_receiver_name() {
        let (provider, _state) = AirPlayProvider::testing();
        assert_eq!(provider.name(), "airplay");
        assert_eq!(provider.receiver_name().as_deref(), Some("Herdr Music"));
    }

    #[test]
    fn verbs_fail_with_no_active_device_without_a_session() {
        let (provider, _state) = AirPlayProvider::testing();
        assert_eq!(provider.play().unwrap_err(), ProviderError::NoActiveDevice);
        assert_eq!(provider.pause().unwrap_err(), ProviderError::NoActiveDevice);
        assert_eq!(provider.next().unwrap_err(), ProviderError::NoActiveDevice);
        assert_eq!(
            provider.previous().unwrap_err(),
            ProviderError::NoActiveDevice
        );
        assert_eq!(
            provider.seek(Duration::from_secs(10)).unwrap_err(),
            ProviderError::NoActiveDevice
        );
        assert_eq!(
            provider.seek_relative(10).unwrap_err(),
            ProviderError::NoActiveDevice
        );
    }

    #[test]
    fn verbs_reach_the_sender_control() {
        let (provider, state) = AirPlayProvider::testing();
        let control = RecordingControl::new();
        connect(&state);
        send(&state, SenderEvent::RemoteAvailable(control.clone()));
        provider.play().unwrap();
        progress(&state); // the sender reports playing…
        provider.pause().unwrap();
        provider.next().unwrap();
        provider.previous().unwrap();
        provider.seek(Duration::from_secs(60)).unwrap();
        assert_eq!(
            control.calls(),
            vec!["play", "pause", "next", "previous", "seek:60"]
        );
    }

    #[test]
    fn play_pause_send_the_toggle_only_when_it_changes_state() {
        let (provider, state) = AirPlayProvider::testing();
        let control = RecordingControl::new();
        connect(&state);
        send(&state, SenderEvent::RemoteAvailable(control.clone()));
        // DACP has only a `playpause` toggle: `s` on a paused session
        // must not restart it, `play` while playing must not pause it.
        provider.pause().unwrap();
        assert!(control.calls().is_empty());
        provider.play().unwrap();
        assert_eq!(control.calls(), vec!["play"]);
        // The sender reports playing: pause toggles, play does not.
        progress(&state);
        provider.play().unwrap();
        provider.pause().unwrap();
        assert_eq!(control.calls(), vec!["play", "pause"]);
        // The sender reports paused again: another `s` is a no-op.
        send(&state, SenderEvent::Flush);
        provider.pause().unwrap();
        assert_eq!(control.calls(), vec!["play", "pause"]);
    }

    #[test]
    fn dacp_commands_translate_trait_verbs() {
        let remote = Arc::new(FakeRemote {
            sent: Mutex::new(Vec::new()),
        });
        let control = DacpSenderControl::new(remote.clone());
        control.play().unwrap();
        control.pause().unwrap();
        control.next().unwrap();
        control.previous().unwrap();
        assert_eq!(
            *remote.sent.lock().unwrap(),
            vec![
                shairplay::RemoteCommand::Play,
                shairplay::RemoteCommand::Pause,
                shairplay::RemoteCommand::NextTrack,
                shairplay::RemoteCommand::PreviousTrack,
            ]
        );
    }

    #[test]
    fn seek_has_no_dacp_command() {
        // shairplay's RemoteCommand set carries no seek; the verb fails
        // honestly instead of silently doing nothing.
        let remote = Arc::new(FakeRemote {
            sent: Mutex::new(Vec::new()),
        });
        let control = DacpSenderControl::new(remote);
        let err = control.seek(Duration::from_secs(30)).unwrap_err();
        assert!(err.to_string().contains("not supported"), "{err}");
    }

    #[test]
    fn playback_is_none_until_a_sender_connects() {
        let (provider, state) = AirPlayProvider::testing();
        assert_eq!(provider.playback().unwrap(), None);
        connect(&state);
        send(
            &state,
            SenderEvent::Metadata {
                title: Some("Song".into()),
                artist: Some("Artist".into()),
                album: Some("Album".into()),
                duration_ms: Some(200_000),
            },
        );
        send(
            &state,
            SenderEvent::Progress {
                start: 0,
                current: 60 * 44_100,
                end: 200 * 44_100,
            },
        );
        let pb = provider.playback().unwrap().unwrap();
        assert_eq!(pb.track.title, "Song");
        // Interpolated to wall-clock now — assert whole seconds.
        assert_eq!(pb.progress.as_secs(), 60);
        assert!(pb.is_playing);
        send(
            &state,
            SenderEvent::Disconnected {
                addr: "10.0.0.4".into(),
            },
        );
        assert_eq!(provider.playback().unwrap(), None);
    }

    #[test]
    fn devices_row_is_the_receiver_and_tracks_connection() {
        let (provider, state) = AirPlayProvider::testing();
        assert_eq!(
            provider.devices().unwrap(),
            vec![Device {
                id: "airplay".into(),
                name: "Herdr Music (AirPlay)".into(),
                is_active: false,
            }]
        );
        connect(&state);
        assert!(provider.devices().unwrap()[0].is_active);
    }

    #[test]
    fn transfer_accepts_only_the_receiver_row() {
        let (provider, _state) = AirPlayProvider::testing();
        assert!(provider.transfer("airplay").is_ok());
        assert!(provider.transfer("kitchen").is_err());
    }
}
