use crate::provider::{Device, MusicProvider, Playback, ProviderError, SEEK_STEP_SECS};
use std::time::Duration;

/// Every transport/UI intent, produced by key events or `ctl` argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Toggle,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    SeekForward,
    SeekBack,
    /// Absolute position, e.g. from a click on the scrub bar.
    SeekTo(Duration),
    Refresh,
    /// Toggle the device picker.
    Devices,
    /// Move the picker selection by `n` rows.
    PickerMove(i32),
    /// Activate the highlighted picker row (transfer to that device).
    PickerSelect,
    /// Transfer playback to a device id.
    SelectDevice(String),
    Quit,
}

/// Point-in-time copy of what the UI needs to render.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub playback: Option<Playback>,
    pub status: Option<String>,
    pub devices: Vec<Device>,
    pub picker_open: bool,
    pub picker_index: usize,
}

/// Provider-agnostic controller: maps commands onto a `MusicProvider`,
/// holds the last playback snapshot and a transient status message.
/// Used directly by `ctl` (single-threaded) and by the TUI worker thread.
#[derive(Default)]
pub struct Controller {
    playback: Option<Playback>,
    status: Option<String>,
    devices: Vec<Device>,
    picker_open: bool,
    picker_index: usize,
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
            Command::SeekTo(pos) => self.run(provider, |p| p.seek(pos)),
            Command::Devices => {
                self.picker_open = !self.picker_open;
                if self.picker_open {
                    self.refresh(provider);
                }
            }
            Command::PickerMove(delta) => {
                let rows = self.devices.len().max(1) as i32;
                self.picker_index = (self.picker_index as i32 + delta).clamp(0, rows - 1) as usize;
            }
            Command::SelectDevice(id) => {
                self.run(provider, |p| p.transfer(&id));
                self.picker_open = false;
            }
            Command::PickerSelect => {
                if let Some(id) = self.devices.get(self.picker_index).map(|d| d.id.clone()) {
                    return self.handle(Command::SelectDevice(id), provider);
                }
            }
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

    /// Devices from the current state — the same list `draw` shows
    /// and `click` resolves.
    pub fn rows(&self) -> &[Device] {
        &self.devices
    }

    pub fn refresh(&mut self, provider: &dyn MusicProvider) {
        match provider.playback() {
            Ok(pb) => {
                self.playback = pb;
                self.status = None;
            }
            Err(e) => self.status = Some(e.to_string()),
        }
        // The picker needs a fresh device list while it's open.
        if self.picker_open {
            if let Ok(devices) = provider.devices() {
                self.devices = devices;
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            playback: self.playback.clone(),
            status: self.status.clone(),
            devices: self.devices.clone(),
            picker_open: self.picker_open,
            picker_index: self.picker_index,
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

    fn device(id: &str, name: &str, active: bool) -> Device {
        Device {
            id: id.into(),
            name: name.into(),
            is_active: active,
        }
    }

    #[test]
    fn quit_returns_true_and_does_not_call_provider() {
        let fake = FakeProvider::new();
        let mut c = Controller::new();
        assert!(c.handle(Command::Quit, &fake));
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn seek_to_seeks_to_absolute_position() {
        let fake = FakeProvider::new();
        let mut c = Controller::new();
        c.handle(Command::SeekTo(Duration::from_secs(42)), &fake);
        assert_eq!(fake.calls(), vec!["seek:42"]);
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

    #[test]
    fn devices_fills_the_snapshot_list() {
        let fake = FakeProvider::new();
        fake.set_devices(vec![device("d1", "Kitchen", true)]);
        let mut c = Controller::new();
        c.handle(Command::Devices, &fake);
        let snap = c.snapshot();
        assert!(snap.picker_open);
        assert_eq!(snap.devices, vec![device("d1", "Kitchen", true)]);
    }

    #[test]
    fn select_device_transfers_then_refreshes() {
        let fake = FakeProvider::new();
        let mut c = Controller::new();
        c.handle(Command::Devices, &fake); // open the picker
        c.handle(Command::SelectDevice("d9".into()), &fake);
        assert_eq!(fake.calls(), vec!["transfer:d9"]);
        assert!(!c.snapshot().picker_open);
    }

    #[test]
    fn picker_select_transfers_the_highlighted_device() {
        let fake = FakeProvider::new();
        fake.set_devices(vec![
            device("d1", "Herdr Music", false),
            device("d2", "Kitchen", false),
        ]);
        let mut c = Controller::new();
        c.handle(Command::Devices, &fake); // open: fills the device list
        c.handle(Command::PickerMove(1), &fake);
        c.handle(Command::PickerSelect, &fake);
        assert_eq!(fake.calls(), vec!["transfer:d2"]);
        assert!(!c.snapshot().picker_open);
    }

    #[test]
    fn picker_move_clamps_to_row_bounds() {
        let fake = FakeProvider::new();
        fake.set_devices(vec![
            device("d1", "Herdr Music", false),
            device("d2", "Kitchen", false),
        ]);
        let mut c = Controller::new();
        c.handle(Command::Devices, &fake);
        c.handle(Command::PickerMove(1), &fake);
        c.handle(Command::PickerMove(5), &fake);
        assert_eq!(c.snapshot().picker_index, 1);
        c.handle(Command::PickerMove(-9), &fake);
        assert_eq!(c.snapshot().picker_index, 0);
    }
}
