use crate::provider::{MusicProvider, Playback, ProviderError, SEEK_STEP_SECS};

/// Every transport/UI intent, produced by key events or `ctl` argv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Toggle,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    SeekForward,
    SeekBack,
    Refresh,
    Quit,
}

/// Point-in-time copy of what the UI needs to render.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub playback: Option<Playback>,
    pub status: Option<String>,
}

/// Provider-agnostic controller: maps commands onto a `MusicProvider`,
/// holds the last playback snapshot and a transient status message.
/// Used directly by `ctl` (single-threaded) and by the TUI worker thread.
#[derive(Default)]
pub struct Controller {
    playback: Option<Playback>,
    status: Option<String>,
}

impl Controller {
    pub fn new() -> Self {
        Self::default()
    }

    /// Execute a command against the provider. Returns `true` only for
    /// `Quit`, so callers can `if handle(..) { break }`.
    pub fn handle(&mut self, cmd: Command, provider: &dyn MusicProvider) -> bool {
        match cmd {
            Command::Quit => return true,
            Command::Refresh => self.refresh(provider),
            Command::Toggle => self.run(provider, |p| p.play_pause()),
            Command::Play => self.run(provider, |p| p.play()),
            Command::Pause | Command::Stop => self.run(provider, |p| p.pause()),
            Command::Next => self.run(provider, |p| p.next()),
            Command::Previous => self.run(provider, |p| p.previous()),
            Command::SeekForward => self.run(provider, |p| p.seek_relative(SEEK_STEP_SECS)),
            Command::SeekBack => self.run(provider, |p| p.seek_relative(-SEEK_STEP_SECS)),
        }
        false
    }

    fn run(
        &mut self,
        provider: &dyn MusicProvider,
        f: impl FnOnce(&dyn MusicProvider) -> Result<(), ProviderError>,
    ) {
        match f(provider) {
            Ok(()) => self.refresh(provider),
            Err(e) => self.status = Some(e.to_string()),
        }
    }

    pub fn refresh(&mut self, provider: &dyn MusicProvider) {
        match provider.playback() {
            Ok(pb) => {
                self.playback = pb;
                self.status = None;
            }
            Err(e) => self.status = Some(e.to_string()),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            playback: self.playback.clone(),
            status: self.status.clone(),
        }
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::testing::*;

    #[test]
    fn quit_returns_true_and_does_not_call_provider() {
        let fake = FakeProvider::new();
        let mut c = Controller::new();
        assert!(c.handle(Command::Quit, &fake));
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn stop_maps_to_pause() {
        let fake = FakeProvider::new();
        let mut c = Controller::new();
        c.handle(Command::Stop, &fake);
        assert_eq!(fake.calls(), vec!["pause"]);
    }

    #[test]
    fn successful_command_refreshes_playback() {
        let fake = FakeProvider::new();
        fake.push_playback(Some(sample_playback())); // consumed by toggle's playback()
        fake.push_playback(Some(sample_playback())); // consumed by refresh
        let mut c = Controller::new();
        c.handle(Command::Toggle, &fake);
        assert_eq!(fake.calls(), vec!["pause"]); // sample is playing
        assert!(c.snapshot().playback.is_some());
        assert!(c.status().is_none());
    }

    #[test]
    fn provider_error_lands_on_status() {
        let fake = FakeProvider::new();
        *fake.error.lock().unwrap() = Some(ProviderError::NotAuthenticated);
        let mut c = Controller::new();
        c.handle(Command::Toggle, &fake);
        assert!(c.status().unwrap().contains("not connected"));
    }

    #[test]
    fn seek_commands_use_step_and_clamp() {
        let fake = FakeProvider::new();
        let mut pb = sample_playback();
        pb.progress = std::time::Duration::from_secs(195);
        fake.push_playback(Some(pb)); // for seek_relative's playback()
        let mut c = Controller::new();
        c.handle(Command::SeekForward, &fake);
        assert_eq!(fake.calls(), vec!["seek:200"]);
    }
}
