use std::fmt;
use std::time::Duration;

/// How far a single forward/rewind command moves playback.
pub const SEEK_STEP_SECS: i64 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub duration: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playback {
    pub track: Track,
    pub progress: Duration,
    pub is_playing: bool,
    pub device: Option<String>,
}

impl Playback {
    /// Progress `elapsed` wall-clock time after this snapshot was observed.
    /// Playing snapshots advance (clamped to the track duration); paused
    /// snapshots stay put.
    pub fn progress_at(&self, elapsed: Duration) -> Duration {
        if !self.is_playing {
            return self.progress;
        }
        (self.progress + elapsed).min(self.track.duration)
    }
}

/// A Spotify Connect device the user can transfer playback to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    NotAuthenticated,
    NoActiveDevice,
    PremiumRequired,
    Api(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAuthenticated => {
                write!(f, "not connected — run `herdr-music auth login`")
            }
            Self::NoActiveDevice => {
                write!(f, "no active device — start playback on a device first")
            }
            Self::PremiumRequired => {
                write!(f, "playback control requires a Premium account")
            }
            Self::Api(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ProviderError {}

/// Clamp `progress + delta_secs` into `[0, duration]`.
pub fn seek_clamped(progress: Duration, delta_secs: i64, duration: Duration) -> Duration {
    let target = progress.as_secs() as i64 + delta_secs;
    Duration::from_secs(target.clamp(0, duration.as_secs() as i64) as u64)
}

/// The single seam between the plugin and a music service.
/// Everything above this trait — TUI, ctl commands, status — is
/// provider-agnostic; each service is an adapter behind it.
pub trait MusicProvider: Send {
    fn name(&self) -> &'static str;

    /// `Ok(None)` means nothing is currently playing.
    fn playback(&self) -> Result<Option<Playback>, ProviderError>;

    fn play(&self) -> Result<(), ProviderError>;
    fn pause(&self) -> Result<(), ProviderError>;
    fn next(&self) -> Result<(), ProviderError>;
    fn previous(&self) -> Result<(), ProviderError>;
    fn seek(&self, position: Duration) -> Result<(), ProviderError>;

    /// Toggle play/pause based on the current state.
    fn play_pause(&self) -> Result<(), ProviderError> {
        match self.playback()? {
            Some(pb) if pb.is_playing => self.pause(),
            _ => self.play(),
        }
    }

    /// Seek `delta_secs` relative to the current position, clamped to the
    /// track bounds. Positive moves forward, negative rewinds.
    fn seek_relative(&self, delta_secs: i64) -> Result<(), ProviderError> {
        let pb = self.playback()?.ok_or(ProviderError::NoActiveDevice)?;
        self.seek(seek_clamped(pb.progress, delta_secs, pb.track.duration))
    }

    /// Every Connect device on the account.
    fn devices(&self) -> Result<Vec<Device>, ProviderError>;

    /// Move playback to `device_id`, keeping it playing if it was.
    fn transfer(&self, device_id: &str) -> Result<(), ProviderError>;

    /// The name this provider advertises to senders, when it is itself a
    /// receiver (AirPlay). Remote-control providers return `None`.
    fn receiver_name(&self) -> Option<String> {
        None
    }
}

/// A provider that always fails with `message` — surfaces a load-time
/// error inside the TUI's status line instead of exiting the pane.
pub struct ErrorProvider {
    message: String,
}

impl ErrorProvider {
    pub fn new(message: String) -> Self {
        Self { message }
    }

    fn err(&self) -> ProviderError {
        ProviderError::Api(self.message.clone())
    }
}

impl MusicProvider for ErrorProvider {
    fn name(&self) -> &'static str {
        "herdr-music"
    }
    fn playback(&self) -> Result<Option<Playback>, ProviderError> {
        Err(self.err())
    }
    fn play(&self) -> Result<(), ProviderError> {
        Err(self.err())
    }
    fn pause(&self) -> Result<(), ProviderError> {
        Err(self.err())
    }
    fn next(&self) -> Result<(), ProviderError> {
        Err(self.err())
    }
    fn previous(&self) -> Result<(), ProviderError> {
        Err(self.err())
    }
    fn seek(&self, _position: Duration) -> Result<(), ProviderError> {
        Err(self.err())
    }
    fn devices(&self) -> Result<Vec<Device>, ProviderError> {
        Err(self.err())
    }
    fn transfer(&self, _device_id: &str) -> Result<(), ProviderError> {
        Err(self.err())
    }
}

/// Build the adapter `cfg.provider` selects. `airplay` is injectable so
/// tests can assert the dispatch without spawning the receiver (which
/// would bind its RAOP ports).
pub fn from_config(
    cfg: &crate::config::AppConfig,
) -> Result<Box<dyn MusicProvider>, ProviderError> {
    from_config_with(cfg, |c| {
        crate::airplay::provider_from_config(c).map(|p| Box::new(p) as Box<dyn MusicProvider>)
    })
}

fn from_config_with(
    cfg: &crate::config::AppConfig,
    airplay: impl FnOnce(&crate::config::AppConfig) -> Result<Box<dyn MusicProvider>, ProviderError>,
) -> Result<Box<dyn MusicProvider>, ProviderError> {
    match cfg.provider {
        crate::config::ProviderKind::Spotify => {
            crate::spotify::provider_from_config(cfg).map(|p| Box::new(p) as Box<dyn MusicProvider>)
        }
        crate::config::ProviderKind::AirPlay => airplay(cfg),
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// In-memory adapter for tests above the seam. `playback()` pops from
    /// `responses` (defaults to `Ok(None)` when empty); every transport call
    /// is recorded in `calls`. Setting `error` makes all calls fail.
    pub struct FakeProvider {
        pub responses: Mutex<VecDeque<Result<Option<Playback>, ProviderError>>>,
        pub calls: Mutex<Vec<String>>,
        pub error: Mutex<Option<ProviderError>>,
        pub device_list: Mutex<Vec<Device>>,
    }

    impl FakeProvider {
        pub fn new() -> Self {
            Self {
                responses: Mutex::new(VecDeque::new()),
                calls: Mutex::new(Vec::new()),
                error: Mutex::new(None),
                device_list: Mutex::new(Vec::new()),
            }
        }

        pub fn set_devices(&self, devices: Vec<Device>) {
            *self.device_list.lock().unwrap() = devices;
        }

        pub fn push_playback(&self, pb: Option<Playback>) {
            self.responses.lock().unwrap().push_back(Ok(pb));
        }

        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn check(&self) -> Result<(), ProviderError> {
            match &*self.error.lock().unwrap() {
                Some(e) => Err(e.clone()),
                None => Ok(()),
            }
        }
    }

    pub fn sample_playback() -> Playback {
        Playback {
            track: Track {
                title: "Song".into(),
                artists: vec!["Artist".into()],
                album: "Album".into(),
                duration: Duration::from_secs(200),
            },
            progress: Duration::from_secs(60),
            is_playing: true,
            device: Some("Mac".into()),
        }
    }

    impl MusicProvider for FakeProvider {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn playback(&self) -> Result<Option<Playback>, ProviderError> {
            self.check()?;
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(None))?)
        }

        fn play(&self) -> Result<(), ProviderError> {
            self.check()?;
            self.calls.lock().unwrap().push("play".into());
            Ok(())
        }
        fn pause(&self) -> Result<(), ProviderError> {
            self.check()?;
            self.calls.lock().unwrap().push("pause".into());
            Ok(())
        }
        fn next(&self) -> Result<(), ProviderError> {
            self.check()?;
            self.calls.lock().unwrap().push("next".into());
            Ok(())
        }
        fn previous(&self) -> Result<(), ProviderError> {
            self.check()?;
            self.calls.lock().unwrap().push("previous".into());
            Ok(())
        }
        fn seek(&self, position: Duration) -> Result<(), ProviderError> {
            self.check()?;
            self.calls
                .lock()
                .unwrap()
                .push(format!("seek:{}", position.as_secs()));
            Ok(())
        }

        fn devices(&self) -> Result<Vec<Device>, ProviderError> {
            self.check()?;
            Ok(self.device_list.lock().unwrap().clone())
        }

        fn transfer(&self, device_id: &str) -> Result<(), ProviderError> {
            self.check()?;
            self.calls
                .lock()
                .unwrap()
                .push(format!("transfer:{device_id}"));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[test]
    fn progress_advances_only_while_playing() {
        let mut pb = sample_playback();
        assert_eq!(
            pb.progress_at(Duration::from_secs(10)),
            Duration::from_secs(70)
        );
        pb.is_playing = false;
        assert_eq!(
            pb.progress_at(Duration::from_secs(10)),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn progress_clamps_to_track_end() {
        let pb = sample_playback();
        assert_eq!(
            pb.progress_at(Duration::from_secs(999)),
            Duration::from_secs(200)
        );
    }

    #[test]
    fn seek_clamped_stays_within_bounds() {
        let d = Duration::from_secs(200);
        assert_eq!(
            seek_clamped(Duration::from_secs(60), 10, d),
            Duration::from_secs(70)
        );
        assert_eq!(seek_clamped(Duration::from_secs(5), -10, d), Duration::ZERO);
        assert_eq!(
            seek_clamped(Duration::from_secs(195), 10, d),
            Duration::from_secs(200)
        );
    }

    #[test]
    fn play_pause_toggles_on_state() {
        let fake = FakeProvider::new();
        let mut pb = sample_playback();
        fake.push_playback(Some(pb.clone()));
        fake.play_pause().unwrap();
        assert_eq!(fake.calls(), vec!["pause"]);

        pb.is_playing = false;
        fake.push_playback(Some(pb));
        fake.play_pause().unwrap();
        assert_eq!(fake.calls(), vec!["pause", "play"]);
    }

    #[test]
    fn seek_relative_uses_clamped_position() {
        let fake = FakeProvider::new();
        let mut pb = sample_playback();
        pb.progress = Duration::from_secs(195);
        fake.push_playback(Some(pb));
        fake.seek_relative(10).unwrap();
        assert_eq!(fake.calls(), vec!["seek:200"]);

        let mut pb = sample_playback();
        pb.progress = Duration::from_secs(5);
        fake.push_playback(Some(pb));
        fake.seek_relative(-10).unwrap();
        assert_eq!(fake.calls(), vec!["seek:200", "seek:0"]);
    }

    #[test]
    fn seek_relative_without_playback_is_no_active_device() {
        let fake = FakeProvider::new();
        assert_eq!(
            fake.seek_relative(10).unwrap_err(),
            ProviderError::NoActiveDevice
        );
    }

    fn cfg(provider: crate::config::ProviderKind) -> crate::config::AppConfig {
        crate::config::AppConfig {
            spotify_client_id: Some("id".into()),
            provider,
            airplay_name: "Herdr Music".into(),
        }
    }

    #[test]
    fn from_config_picks_spotify_by_default() {
        let provider = from_config_with(&cfg(crate::config::ProviderKind::Spotify), |_| {
            panic!("airplay constructor must not run for spotify config")
        })
        .unwrap();
        assert_eq!(provider.name(), "spotify");
    }

    #[test]
    fn from_config_picks_airplay_for_airplay_cfg() {
        let provider = from_config_with(&cfg(crate::config::ProviderKind::AirPlay), |_| {
            Ok(Box::new(FakeProvider::new()))
        })
        .unwrap();
        assert_eq!(provider.name(), "fake");
    }

    #[test]
    fn error_provider_reports_the_load_error() {
        let p = ErrorProvider::new("bad config".into());
        assert_eq!(p.name(), "herdr-music");
        assert_eq!(
            p.playback().unwrap_err(),
            ProviderError::Api("bad config".into())
        );
        assert_eq!(
            p.next().unwrap_err(),
            ProviderError::Api("bad config".into())
        );
    }
}
