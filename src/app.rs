use crate::provider::{
    Device, LocalStatus, MusicProvider, Playback, ProviderError, SEEK_STEP_SECS,
};
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
    /// Activate the highlighted picker row (transfer, restart, or nothing).
    PickerSelect,
    /// Transfer playback to a device id.
    SelectDevice(String),
    /// Start (or restart) the local playback engine.
    StartLocal,
    Quit,
}

/// What a picker row does when selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    Transfer(String),
    StartLocal,
    Nothing,
}

/// One row of the device picker, shared by draw and click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerRow {
    pub label: String,
    pub active: bool,
    pub action: PickerAction,
}

/// Picker contents: every device, plus a synthetic local row when the
/// configured local device isn't in the account's list yet (still
/// starting, failed, or disabled).
pub fn picker_rows(
    devices: &[Device],
    local_name: Option<&str>,
    local: &LocalStatus,
) -> Vec<PickerRow> {
    let mut rows: Vec<PickerRow> = devices
        .iter()
        .map(|d| {
            let local = Some(d.name.as_str()) == local_name;
            PickerRow {
                label: if local {
                    format!("{} (this laptop)", d.name)
                } else {
                    d.name.clone()
                },
                active: d.is_active,
                action: PickerAction::Transfer(d.id.clone()),
            }
        })
        .collect();
    let Some(name) = local_name else {
        return rows;
    };
    if devices.iter().any(|d| d.name == name) {
        return rows;
    }
    let (suffix, action) = match local {
        LocalStatus::Failed(_) => (" — restart", PickerAction::StartLocal),
        LocalStatus::Disabled { reason, .. } => {
            return rows
                .into_iter()
                .chain([PickerRow {
                    label: format!("{name} (this laptop — {reason})"),
                    active: false,
                    action: PickerAction::Nothing,
                }])
                .collect()
        }
        LocalStatus::NotStarted => ("", PickerAction::StartLocal),
        LocalStatus::Starting | LocalStatus::Ready => (" — starting…", PickerAction::Nothing),
    };
    rows.push(PickerRow {
        label: format!("{name} (this laptop{suffix})"),
        active: false,
        action,
    });
    rows
}

/// Point-in-time copy of what the UI needs to render.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub playback: Option<Playback>,
    pub status: Option<String>,
    pub devices: Vec<Device>,
    pub local_name: Option<String>,
    pub local_status: LocalStatus,
    /// Engine state worth a line of the UI: "starting…", a failure, or a
    /// non-silent disabled reason. None once dismissed via the picker.
    pub local_alert: Option<String>,
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
    local_name: Option<String>,
    local_status: LocalStatus,
    /// A Failed/Disabled alert hides again once the picker has been opened.
    alert_dismissed: bool,
    picker_open: bool,
    picker_index: usize,
    /// Auto-transfer to the local device happens once per session.
    transfer_attempted: bool,
}

impl Controller {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable local playback under `name` (None = disabled in config).
    pub fn with_local(mut self, name: Option<String>) -> Self {
        self.local_name = name;
        self
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
                    // Opening the picker acknowledges the current alert.
                    self.alert_dismissed = true;
                    self.refresh(provider);
                }
            }
            Command::PickerMove(delta) => {
                let rows = self.rows().len().max(1) as i32;
                self.picker_index = (self.picker_index as i32 + delta).clamp(0, rows - 1) as usize;
            }
            Command::SelectDevice(id) => {
                self.run(provider, |p| p.transfer(&id));
                self.picker_open = false;
            }
            Command::PickerSelect => {
                let action = self.rows().get(self.picker_index).map(|r| r.action.clone());
                match action {
                    Some(PickerAction::Transfer(id)) => {
                        self.run(provider, |p| p.transfer(&id));
                        self.picker_open = false;
                    }
                    Some(PickerAction::StartLocal) => {
                        return self.handle(Command::StartLocal, provider);
                    }
                    _ => {}
                }
            }
            Command::StartLocal => {
                let failed = match self.local_name.clone() {
                    Some(name) => provider.start_local(&name).err().map(|e| e.to_string()),
                    None => {
                        self.local_status = LocalStatus::Disabled {
                            reason: "local playback disabled in config.toml".into(),
                            silent: true,
                        };
                        None
                    }
                };
                self.picker_open = false;
                // A fresh engine gets a fresh auto-transfer when it's ready.
                self.transfer_attempted = false;
                self.refresh(provider);
                // refresh() re-polls local_status(); apply a start error
                // after it so the failure isn't overwritten by the poll.
                if let Some(msg) = failed {
                    self.local_status = LocalStatus::Failed(msg);
                    self.alert_dismissed = false;
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

    /// Picker rows from the current state — the same list `draw` shows
    /// and `click` resolves.
    pub fn rows(&self) -> Vec<PickerRow> {
        picker_rows(
            &self.devices,
            self.local_name.as_deref(),
            &self.local_status,
        )
    }

    pub fn refresh(&mut self, provider: &dyn MusicProvider) {
        match provider.playback() {
            Ok(pb) => {
                self.playback = pb;
                self.status = None;
            }
            Err(e) => self.status = Some(e.to_string()),
        }
        let previous = self.local_status.clone();
        self.local_status = provider.local_status();
        // A fresh failure or disable re-arms the alert.
        let alert =
            |s: &LocalStatus| matches!(s, LocalStatus::Failed(_) | LocalStatus::Disabled { .. });
        if !alert(&previous) && alert(&self.local_status) {
            self.alert_dismissed = false;
        }
        // The picker always needs a fresh list; auto-transfer needs it on
        // the tick the engine becomes ready. Skip the request otherwise.
        if self.picker_open || (!self.transfer_attempted && self.local_status == LocalStatus::Ready)
        {
            if let Ok(devices) = provider.devices() {
                self.devices = devices;
            }
        }
        // If the engine just came up and nothing is playing anywhere,
        // hand playback to it once so pressing play makes sound here.
        if !self.transfer_attempted
            && self.local_status == LocalStatus::Ready
            && self.playback.is_none()
        {
            self.transfer_attempted = true;
            let id = self
                .devices
                .iter()
                .find(|d| Some(d.name.as_str()) == self.local_name.as_deref())
                .map(|d| d.id.clone());
            if let Some(id) = id {
                if let Err(e) = provider.transfer(&id) {
                    self.status = Some(e.to_string());
                }
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let local_alert = match &self.local_status {
            LocalStatus::Starting => Some("starting local playback…".to_string()),
            LocalStatus::Failed(r) if !self.alert_dismissed => Some(r.clone()),
            LocalStatus::Disabled { reason, silent } if !*silent && !self.alert_dismissed => {
                Some(reason.clone())
            }
            _ => None,
        };
        Snapshot {
            playback: self.playback.clone(),
            status: self.status.clone(),
            devices: self.devices.clone(),
            local_name: self.local_name.clone(),
            local_status: self.local_status.clone(),
            local_alert,
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

    fn local_controller() -> (FakeProvider, Controller) {
        (
            FakeProvider::new(),
            Controller::new().with_local(Some("Herdr Music".into())),
        )
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
    fn start_local_records_call_and_status_lands_on_snapshot() {
        let (fake, mut c) = local_controller();
        fake.set_local_status(LocalStatus::Starting);
        c.handle(Command::StartLocal, &fake);
        assert_eq!(fake.calls(), vec!["start_local:Herdr Music"]);
        let snap = c.snapshot();
        assert_eq!(snap.local_status, LocalStatus::Starting);
        assert_eq!(
            snap.local_alert.as_deref(),
            Some("starting local playback…")
        );
    }

    #[test]
    fn start_local_error_becomes_a_status_not_a_panic() {
        let (fake, mut c) = local_controller();
        *fake.start_local_error.lock().unwrap() =
            Some(ProviderError::LocalPlayback("engine exploded".into()));
        c.handle(Command::StartLocal, &fake);
        let snap = c.snapshot();
        assert_eq!(snap.local_alert.as_deref(), Some("engine exploded"));
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
    fn auto_transfer_once_when_ready_and_nothing_playing() {
        let (fake, mut c) = local_controller();
        fake.set_devices(vec![device("d1", "Herdr Music", false)]);
        fake.set_local_status(LocalStatus::Ready);
        c.handle(Command::StartLocal, &fake);
        assert_eq!(fake.calls(), vec!["start_local:Herdr Music", "transfer:d1"]);

        // Already attempted: a later refresh must not transfer again.
        c.handle(Command::Refresh, &fake);
        assert_eq!(fake.calls(), vec!["start_local:Herdr Music", "transfer:d1"]);
    }

    #[test]
    fn no_auto_transfer_while_something_plays() {
        let (fake, mut c) = local_controller();
        fake.push_playback(Some(sample_playback()));
        fake.set_devices(vec![device("d1", "Herdr Music", false)]);
        fake.set_local_status(LocalStatus::Ready);
        c.handle(Command::StartLocal, &fake);
        assert_eq!(fake.calls(), vec!["start_local:Herdr Music"]);
    }

    #[test]
    fn picker_rows_label_local_and_mark_active() {
        let devices = vec![
            device("d1", "Herdr Music", false),
            device("d2", "Kitchen", true),
        ];
        let rows = picker_rows(&devices, Some("Herdr Music"), &LocalStatus::Ready);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "Herdr Music (this laptop)");
        assert_eq!(rows[0].action, PickerAction::Transfer("d1".into()));
        assert!(rows[1].active);
    }

    #[test]
    fn picker_rows_offer_restart_when_engine_failed() {
        let rows = picker_rows(
            &[],
            Some("Herdr Music"),
            &LocalStatus::Failed("dead".into()),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Herdr Music (this laptop — restart)");
        assert_eq!(rows[0].action, PickerAction::StartLocal);
    }

    #[test]
    fn picker_rows_explain_scope_disable() {
        let disabled = LocalStatus::Disabled {
            reason: "re-run `herdr-music auth login` to enable local playback".into(),
            silent: true,
        };
        let rows = picker_rows(&[], Some("Herdr Music"), &disabled);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].label.contains("re-run `herdr-music auth login`"));
        assert_eq!(rows[0].action, PickerAction::Nothing);
    }

    #[test]
    fn failure_alert_hides_after_picker_opens() {
        let (fake, mut c) = local_controller();
        fake.set_local_status(LocalStatus::Failed("boom".into()));
        c.handle(Command::StartLocal, &fake); // picks up the failed status
        assert_eq!(c.snapshot().local_alert.as_deref(), Some("boom"));
        c.handle(Command::Devices, &fake); // open = dismiss
        c.handle(Command::Devices, &fake); // close again
        assert_eq!(c.snapshot().local_alert, None);
    }

    #[test]
    fn silent_disabled_never_alerts() {
        let (fake, mut c) = local_controller();
        fake.set_local_status(LocalStatus::Disabled {
            reason: "re-run auth login".into(),
            silent: true,
        });
        c.handle(Command::Refresh, &fake);
        assert_eq!(c.snapshot().local_alert, None);
    }

    #[test]
    fn picker_move_clamps_to_row_bounds() {
        let (fake, mut c) = local_controller();
        fake.set_devices(vec![
            device("d1", "Herdr Music", false),
            device("d2", "Kitchen", false),
        ]);
        c.handle(Command::Devices, &fake);
        c.handle(Command::PickerMove(1), &fake);
        c.handle(Command::PickerMove(5), &fake);
        assert_eq!(c.snapshot().picker_index, 1);
        c.handle(Command::PickerMove(-9), &fake);
        assert_eq!(c.snapshot().picker_index, 0);
    }
}
