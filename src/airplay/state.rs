//! The event-driven state machine behind `AirPlayProvider`: sender events
//! from the RAOP runtime land here, and `playback()` maps them onto the
//! shared `Playback` type. Pure — all clock values are injected.

use super::SenderControl;
use crate::provider::{Playback, ProviderError, Track};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// RTP progress timestamps run at 44.1 kHz.
const RTP_RATE: u32 = 44_100;

/// An event the sender (or the receiver itself) reports on the session.
pub enum SenderEvent {
    /// A sender connected; `name` is what it reported (an address for AP1).
    Connected { name: Option<String> },
    /// DMAP track metadata.
    Metadata {
        title: Option<String>,
        artist: Option<String>,
        album: Option<String>,
        duration_ms: Option<u32>,
    },
    /// RTP progress triple (start, current, end) at 44.1 kHz.
    Progress { start: u32, current: u32, end: u32 },
    /// The sender flushed its stream — a pause or a seek boundary.
    Flush,
    /// Sender volume in dB (0 … −30, −144 mute).
    Volume(f32),
    /// A DACP remote-control handle for this session.
    RemoteAvailable(Arc<dyn SenderControl>),
    /// A sender-side TCP connection dropped; `addr` identifies it.
    Disconnected { addr: String },
    /// A receiver-side failure worth showing in the status row.
    Error(String),
}

struct Session {
    device: Option<String>,
    track: Track,
    progress: Duration,
    /// When `progress` was measured — interpolation base.
    progress_at: Instant,
    is_playing: bool,
    control: Option<Arc<dyn SenderControl>>,
}

impl Session {
    fn new(device: Option<String>) -> Self {
        Self {
            device,
            track: Track {
                title: String::new(),
                artists: Vec::new(),
                album: String::new(),
                duration: Duration::ZERO,
            },
            progress: Duration::ZERO,
            progress_at: Instant::now(),
            // Not playing until the first `Progress` says so — a bare
            // connection is not playback.
            is_playing: false,
            control: None,
        }
    }

    /// Position `now` wall-clock after the last progress event: advances
    /// only while playing, clamped to the track duration when known.
    fn position(&self, now: Instant) -> Duration {
        if !self.is_playing {
            return self.progress;
        }
        let progressed = self
            .progress
            .saturating_add(now.saturating_duration_since(self.progress_at));
        if self.track.duration.is_zero() {
            progressed
        } else {
            progressed.min(self.track.duration)
        }
    }

    /// Bake elapsed wall time into `progress` — used before freezing or
    /// replacing the interpolation base.
    fn settle(&mut self, at: Instant) {
        self.progress = self.position(at);
        self.progress_at = at;
    }
}

/// RTP timestamp span (`to − from`) as a duration, clamped at zero: a
/// `current` below `start` is a sender quirk, not a wraparound — the
/// 44.1 kHz counter would need a ~27-hour track to legitimately wrap,
/// so `saturating_sub` keeps a bad triple from becoming a ~27h position.
fn rtp_span(from: u32, to: u32) -> Duration {
    Duration::from_secs_f64(to.saturating_sub(from) as f64 / RTP_RATE as f64)
}

pub struct SessionState {
    session: Option<Session>,
    /// Runtime failure (receiver died, a connection errored). Cleared when
    /// a sender connects successfully.
    error: Option<String>,
    /// Environmental failure that stays: e.g. no player binary on Linux.
    sink_error: Option<String>,
    /// Last reported sender volume, in dB — sessions read the matching
    /// linear gain via [`volume_gain`](Self::volume_gain).
    volume_db: f32,
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            session: None,
            error: None,
            sink_error: None,
            volume_db: 0.0,
        }
    }

    /// `sink_error` sticks once set: the pane keeps accepting sessions but
    /// the status row explains why they are silent.
    ///
    /// `error` is transient by contrast: the next connection, progress
    /// event or disconnect clears it.
    pub fn set_sink_error(&mut self, msg: String) {
        self.sink_error = Some(msg);
    }

    pub fn apply(&mut self, event: SenderEvent, at: Instant) {
        match event {
            SenderEvent::Connected { name } => {
                // shairplay fires this per accepted TCP connection, not
                // per sender session: a second connection (the same
                // phone on another interface, a stray probe) must not
                // wipe the live session's metadata. The session ends
                // when its own connection drops.
                if self.session.is_none() {
                    self.session = Some(Session::new(name));
                }
                self.error = None;
            }
            SenderEvent::Metadata {
                title,
                artist,
                album,
                duration_ms,
            } => {
                if let Some(s) = &mut self.session {
                    // DMAP sends the whole item per track — replace, do
                    // not merge, or the previous track's fields leak
                    // into the next one.
                    s.track = Track {
                        title: title.unwrap_or_default(),
                        artists: artist.into_iter().collect(),
                        album: album.unwrap_or_default(),
                        duration: duration_ms
                            .map_or(Duration::ZERO, |ms| Duration::from_millis(ms.into())),
                    };
                }
            }
            SenderEvent::Progress {
                start,
                current,
                end,
            } => {
                self.error = None;
                if let Some(s) = &mut self.session {
                    s.progress = rtp_span(start, current);
                    s.progress_at = at;
                    s.is_playing = true;
                    // The triple carries the track's span on every
                    // event — a track change must replace the duration,
                    // not only fill it in once.
                    let span = rtp_span(start, end);
                    if !span.is_zero() {
                        s.track.duration = span;
                    }
                }
            }
            SenderEvent::Flush => {
                if let Some(s) = &mut self.session {
                    s.settle(at);
                    s.is_playing = false;
                }
            }
            SenderEvent::Volume(db) => self.volume_db = db,
            SenderEvent::RemoteAvailable(control) => {
                if let Some(s) = &mut self.session {
                    s.control = Some(control);
                }
            }
            SenderEvent::Disconnected { addr } => {
                // Per-connection, like `Connected`: only the session's
                // own connection may end it — a stray connection's
                // teardown must not close a session it never owned.
                // A session without a recorded address is closed by any
                // disconnect (tests create those).
                let owned = self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.device.as_deref().map_or(true, |d| d == addr));
                if owned {
                    self.session = None;
                }
                self.error = None;
            }
            SenderEvent::Error(msg) => self.error = Some(msg),
        }
    }

    /// Latest `Playback`, interpolated to `now`. `Ok(None)` while no
    /// sender is connected; errors beat playback so the status row can
    /// explain a dead receiver or a silent sink.
    pub fn playback(&self, now: Instant) -> Result<Option<Playback>, ProviderError> {
        if let Some(msg) = self.error.clone().or_else(|| self.sink_error.clone()) {
            return Err(ProviderError::Api(msg));
        }
        Ok(self.session.as_ref().map(|s| Playback {
            track: s.track.clone(),
            progress: s.position(now),
            is_playing: s.is_playing,
            device: s.device.clone(),
        }))
    }

    /// Linear gain for the sender's current volume (0 dB → 1.0, −144 → 0).
    pub fn volume_gain(&self) -> f32 {
        super::sink::db_to_gain(self.volume_db)
    }

    /// Whether a sender session is live — drives `devices()` `is_active`.
    pub fn connected(&self) -> bool {
        self.session.is_some()
    }

    /// The session's play state (`false` until the first `Progress`).
    /// `AirPlayProvider` gates `play`/`pause` on it because DACP offers
    /// only a `playpause` toggle.
    pub fn is_playing(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.is_playing)
    }

    /// The session's DACP handle, `NoActiveDevice` without one.
    pub fn control(&self) -> Result<Arc<dyn SenderControl>, ProviderError> {
        self.session
            .as_ref()
            .and_then(|s| s.control.clone())
            .ok_or(ProviderError::NoActiveDevice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoControl;
    impl SenderControl for NoControl {
        fn play(&self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn pause(&self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn next(&self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn previous(&self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn seek(&self, _p: Duration) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    fn meta() -> SenderEvent {
        SenderEvent::Metadata {
            title: Some("Song".into()),
            artist: Some("Artist".into()),
            album: Some("Album".into()),
            duration_ms: Some(200_000),
        }
    }

    #[test]
    fn no_session_is_none() {
        let state = SessionState::new();
        assert_eq!(state.playback(Instant::now()), Ok(None));
    }

    #[test]
    fn metadata_and_progress_map_to_playback() {
        let t0 = Instant::now();
        let mut state = SessionState::new();
        state.apply(
            SenderEvent::Connected {
                name: Some("iPhone".into()),
            },
            t0,
        );
        state.apply(meta(), t0);
        state.apply(
            SenderEvent::Progress {
                start: 0,
                current: 60 * RTP_RATE,
                end: 200 * RTP_RATE,
            },
            t0,
        );
        let pb = state.playback(t0).unwrap().unwrap();
        assert_eq!(pb.track.title, "Song");
        assert_eq!(pb.track.artists, vec!["Artist".to_string()]);
        assert_eq!(pb.track.album, "Album");
        assert_eq!(pb.track.duration, Duration::from_secs(200));
        assert_eq!(pb.progress, Duration::from_secs(60));
        assert!(pb.is_playing);
        assert_eq!(pb.device.as_deref(), Some("iPhone"));
    }

    #[test]
    fn progress_advances_with_elapsed_only_while_playing() {
        let t0 = Instant::now();
        let mut state = SessionState::new();
        state.apply(SenderEvent::Connected { name: None }, t0);
        state.apply(meta(), t0);
        state.apply(
            SenderEvent::Progress {
                start: 0,
                current: 60 * RTP_RATE,
                end: 200 * RTP_RATE,
            },
            t0,
        );
        assert_eq!(
            state
                .playback(t0 + Duration::from_secs(5))
                .unwrap()
                .unwrap()
                .progress,
            Duration::from_secs(65)
        );
        state.apply(SenderEvent::Flush, t0 + Duration::from_secs(10));
        let pb = state
            .playback(t0 + Duration::from_secs(20))
            .unwrap()
            .unwrap();
        assert!(!pb.is_playing);
        assert_eq!(pb.progress, Duration::from_secs(70));
    }

    #[test]
    fn progress_clamps_to_duration() {
        let t0 = Instant::now();
        let mut state = SessionState::new();
        state.apply(SenderEvent::Connected { name: None }, t0);
        state.apply(meta(), t0);
        state.apply(
            SenderEvent::Progress {
                start: 0,
                current: 195 * RTP_RATE,
                end: 200 * RTP_RATE,
            },
            t0,
        );
        let pb = state
            .playback(t0 + Duration::from_secs(30))
            .unwrap()
            .unwrap();
        assert_eq!(pb.progress, Duration::from_secs(200));
    }

    #[test]
    fn sender_side_seek_updates_progress() {
        let t0 = Instant::now();
        let mut state = SessionState::new();
        state.apply(SenderEvent::Connected { name: None }, t0);
        state.apply(meta(), t0);
        state.apply(
            SenderEvent::Progress {
                start: 0,
                current: 60 * RTP_RATE,
                end: 200 * RTP_RATE,
            },
            t0,
        );
        // Sender seeks: progress triple jumps, flush marks the boundary.
        state.apply(SenderEvent::Flush, t0 + Duration::from_secs(1));
        state.apply(
            SenderEvent::Progress {
                start: 0,
                current: 120 * RTP_RATE,
                end: 200 * RTP_RATE,
            },
            t0 + Duration::from_secs(2),
        );
        let pb = state
            .playback(t0 + Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert!(pb.is_playing);
        assert_eq!(pb.progress, Duration::from_secs(120));
    }

    #[test]
    fn disconnect_returns_to_none() {
        let t0 = Instant::now();
        let mut state = SessionState::new();
        state.apply(
            SenderEvent::Connected {
                name: Some("10.0.0.4".into()),
            },
            t0,
        );
        state.apply(meta(), t0);
        state.apply(
            SenderEvent::Disconnected {
                addr: "10.0.0.4".into(),
            },
            t0,
        );
        assert_eq!(state.playback(t0), Ok(None));
        assert!(!state.connected());
    }

    #[test]
    fn a_bare_connection_is_not_playing() {
        let t0 = Instant::now();
        let mut state = SessionState::new();
        state.apply(
            SenderEvent::Connected {
                name: Some("10.0.0.4".into()),
            },
            t0,
        );
        assert!(!state.is_playing());
        assert!(!state.playback(t0).unwrap().unwrap().is_playing);
    }

    #[test]
    fn stray_connections_do_not_wipe_the_session() {
        let t0 = Instant::now();
        let mut state = SessionState::new();
        state.apply(
            SenderEvent::Connected {
                name: Some("10.0.0.4".into()),
            },
            t0,
        );
        state.apply(meta(), t0);
        // A second TCP connection from elsewhere (same phone on another
        // interface, a probe) is a no-op while a session is live…
        state.apply(
            SenderEvent::Connected {
                name: Some("10.0.0.9".into()),
            },
            t0,
        );
        // …and its teardown cannot end the session it never owned.
        state.apply(
            SenderEvent::Disconnected {
                addr: "10.0.0.9".into(),
            },
            t0,
        );
        let pb = state.playback(t0).unwrap().unwrap();
        assert_eq!(pb.device.as_deref(), Some("10.0.0.4"));
        assert!(state.connected());
        // The session's own connection still ends it.
        state.apply(
            SenderEvent::Disconnected {
                addr: "10.0.0.4".into(),
            },
            t0,
        );
        assert_eq!(state.playback(t0), Ok(None));
    }

    #[test]
    fn metadata_replaces_the_previous_track() {
        let t0 = Instant::now();
        let mut state = SessionState::new();
        state.apply(SenderEvent::Connected { name: None }, t0);
        state.apply(meta(), t0);
        // Track change: DMAP sends the new item with no artist/album —
        // the old ones must not linger.
        state.apply(
            SenderEvent::Metadata {
                title: Some("Next Song".into()),
                artist: None,
                album: None,
                duration_ms: None,
            },
            t0,
        );
        let pb = state.playback(t0).unwrap().unwrap();
        assert_eq!(pb.track.title, "Next Song");
        assert!(pb.track.artists.is_empty());
        assert_eq!(pb.track.album, "");
        assert_eq!(pb.track.duration, Duration::ZERO);
    }

    #[test]
    fn progress_updates_duration_on_every_event() {
        let t0 = Instant::now();
        let mut state = SessionState::new();
        state.apply(SenderEvent::Connected { name: None }, t0);
        state.apply(meta(), t0);
        let progress = |end_secs: u32| SenderEvent::Progress {
            start: 0,
            current: 60 * RTP_RATE,
            end: end_secs * RTP_RATE,
        };
        state.apply(progress(200), t0);
        assert_eq!(
            state.playback(t0).unwrap().unwrap().track.duration,
            Duration::from_secs(200)
        );
        // A shorter next track replaces, not only fills in, the duration.
        state.apply(progress(150), t0);
        assert_eq!(
            state.playback(t0).unwrap().unwrap().track.duration,
            Duration::from_secs(150)
        );
    }

    #[test]
    fn a_progress_triple_below_start_is_zero_not_wrapped() {
        let t0 = Instant::now();
        let mut state = SessionState::new();
        state.apply(SenderEvent::Connected { name: None }, t0);
        state.apply(meta(), t0);
        // Sender quirk: `current` below `start`. A wrapping u32 subtract
        // would read ~27h; saturating keeps it at zero.
        state.apply(
            SenderEvent::Progress {
                start: 120 * RTP_RATE,
                current: 60 * RTP_RATE,
                end: 200 * RTP_RATE,
            },
            t0,
        );
        assert_eq!(
            state.playback(t0).unwrap().unwrap().progress,
            Duration::ZERO
        );
    }

    #[test]
    fn runtime_error_clears_on_disconnect() {
        let t0 = Instant::now();
        let mut state = SessionState::new();
        state.apply(SenderEvent::Error("sink died".into()), t0);
        state.apply(
            SenderEvent::Disconnected {
                addr: "10.0.0.4".into(),
            },
            t0,
        );
        assert_eq!(state.playback(t0), Ok(None));
    }

    #[test]
    fn runtime_error_clears_on_next_progress() {
        let t0 = Instant::now();
        let mut state = SessionState::new();
        state.apply(SenderEvent::Connected { name: None }, t0);
        state.apply(SenderEvent::Error("sink died".into()), t0);
        assert!(state.playback(t0).is_err());
        state.apply(
            SenderEvent::Progress {
                start: 0,
                current: 0,
                end: 200 * RTP_RATE,
            },
            t0,
        );
        assert!(state.playback(t0).unwrap().is_some());
    }

    #[test]
    fn control_requires_a_session() {
        let mut state = SessionState::new();
        assert_eq!(state.control().err(), Some(ProviderError::NoActiveDevice));
        let t0 = Instant::now();
        state.apply(SenderEvent::Connected { name: None }, t0);
        // Connected but no DACP handle yet is still NoActiveDevice.
        assert_eq!(state.control().err(), Some(ProviderError::NoActiveDevice));
        state.apply(SenderEvent::RemoteAvailable(Arc::new(NoControl)), t0);
        assert!(state.control().is_ok());
        state.apply(
            SenderEvent::Disconnected {
                addr: "10.0.0.4".into(),
            },
            t0,
        );
        assert_eq!(state.control().err(), Some(ProviderError::NoActiveDevice));
    }

    #[test]
    fn sink_error_is_sticky_and_beats_playback() {
        let t0 = Instant::now();
        let mut state = SessionState::new();
        state.set_sink_error("no player".into());
        state.apply(SenderEvent::Connected { name: None }, t0);
        state.apply(meta(), t0);
        assert_eq!(
            state.playback(t0).unwrap_err(),
            ProviderError::Api("no player".into())
        );
    }

    #[test]
    fn runtime_error_clears_on_connect() {
        let t0 = Instant::now();
        let mut state = SessionState::new();
        state.apply(SenderEvent::Error("bind failed".into()), t0);
        assert_eq!(
            state.playback(t0).unwrap_err(),
            ProviderError::Api("bind failed".into())
        );
        state.apply(SenderEvent::Connected { name: None }, t0);
        assert!(state.playback(t0).unwrap().is_some());
    }
}
