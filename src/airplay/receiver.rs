//! The RAOP server thread: a single OS thread owns a current-thread tokio
//! runtime running the shairplay server. Dropping [`Receiver`] signals
//! shutdown and joins the thread, unadvertising the mDNS name — no child
//! process, no pidfile.
//!
//! tokio is confined to this module; everything else in the adapter is
//! plain std threads.

use super::sink::{self, Sink};
use super::state::{SenderEvent, SessionState};
use super::DacpSenderControl;
use shairplay::{Ap1Encryption, AudioFormat, AudioHandler, AudioSession, RaopServer};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;
use tokio::sync::oneshot;

/// Owns the receiver thread. `Drop` stops the server and joins it.
pub struct Receiver {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    /// PID of the player child the Linux pipe sink is feeding, 0 when
    /// there is none. `Drop` kills it before joining (see below).
    sink_pid: Arc<AtomicU32>,
}

impl Receiver {
    pub fn spawn(
        name: String,
        state: Arc<Mutex<SessionState>>,
        player: sink::PlayerChoice,
    ) -> std::io::Result<Self> {
        let (stop_tx, stop_rx) = oneshot::channel();
        let sink_pid = Arc::new(AtomicU32::new(0));
        let thread = std::thread::Builder::new()
            .name("herdr-music-airplay".into())
            .spawn({
                let sink_pid = sink_pid.clone();
                move || serve(name, state, player, sink_pid, stop_rx)
            })?;
        Ok(Self {
            stop: Some(stop_tx),
            thread: Some(thread),
            sink_pid,
        })
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // The Linux pipe sink writes PCM to the player child's stdin on
        // this thread; a player that stopped consuming fills the pipe
        // and that write blocks forever, so `join` would hang the `q`
        // key. Kill the child first: the write fails EPIPE and the
        // thread unwinds. The pid is 0 on the cpal platforms.
        let pid = self.sink_pid.swap(0, Ordering::Relaxed);
        if pid != 0 {
            kill_player(pid);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// SIGKILL a stuck player child by pid. Only the Linux pipe sink ever
/// stores one; everywhere else the slot stays 0 and this is never
/// reached with a live pid.
#[cfg(unix)]
fn kill_player(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
}

#[cfg(not(unix))]
fn kill_player(_pid: u32) {}

/// Everything past spawn lives on the runtime thread.
fn serve(
    name: String,
    state: Arc<Mutex<SessionState>>,
    player: sink::PlayerChoice,
    sink_pid: Arc<AtomicU32>,
    stop: oneshot::Receiver<()>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            report(&state, format!("airplay runtime: {e}"));
            return;
        }
    };
    runtime.block_on(async move {
        let handler = Arc::new(Handler {
            state: state.clone(),
            player,
            sink_pid,
        });
        let mut server = match build_server(name, handler) {
            Ok(server) => server,
            Err(e) => {
                report(&state, format!("airplay receiver: {e}"));
                return;
            }
        };
        if let Err(e) = server.start().await {
            report(&state, format!("airplay receiver: {e}"));
            return;
        }
        let _ = stop.await;
        server.stop().await;
    });
}

/// A stable 6-byte identity for the advert — random per launch would
/// make the iPhone cache a new dead device every time the pane opens.
/// Locally administered, unicast, derived from the machine's identity.
fn hwaddr() -> Vec<u8> {
    use sha2::Digest;
    let id = ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .iter()
        .find_map(|p| std::fs::read(p).ok().filter(|v| !v.is_empty()))
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| o.stdout)
                .filter(|v| !v.is_empty())
        })
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|v| v.into_encoded_bytes())
                .unwrap_or_default()
        });
    let v = sha2::Sha256::digest(&id);
    vec![(v[0] & !0x01) | 0x02, v[1], v[2], v[3], v[4], v[5]]
}

/// The RAOP server as advertised. `build` binds nothing, so tests can
/// inspect the advert.
fn build_server(
    name: String,
    handler: Arc<dyn AudioHandler>,
) -> Result<RaopServer, shairplay::ShairplayError> {
    RaopServer::builder()
        .name(name)
        .hwaddr(hwaddr())
        .output_sample_rate(44_100)
        .output_max_channels(2)
        // iOS senders stream RSA-encrypted ALAC; the crate's et=0 default makes them fail at play.
        .advertise_encryption([Ap1Encryption::None, Ap1Encryption::Rsa])
        .build(handler)
}

/// Land a receiver-side failure on the status row.
fn report(state: &Arc<Mutex<SessionState>>, msg: String) {
    state
        .lock()
        .unwrap()
        .apply(SenderEvent::Error(msg), Instant::now());
}

/// shairplay event surface → `SessionState`. Every callback applies
/// straight to shared state, stamped when it fires — a queue drained by
/// the 2 s `playback()` poll would report events late and stamp them
/// wrong. All callbacks may run on shairplay's threads.
struct Handler {
    state: Arc<Mutex<SessionState>>,
    player: sink::PlayerChoice,
    /// Shared with [`Receiver`] so drop can kill a stuck player child.
    sink_pid: Arc<AtomicU32>,
}

impl Handler {
    fn apply(&self, event: SenderEvent) {
        self.state.lock().unwrap().apply(event, Instant::now());
    }

    /// A sink for the new stream's format. Failure is reported as a
    /// status-row error; the session is still accepted.
    fn open_sink(&self, format: &AudioFormat) -> Option<Sink> {
        match Sink::open(
            &self.player,
            format,
            self.state.clone(),
            self.sink_pid.clone(),
        ) {
            Ok(sink) => Some(sink),
            Err(e) => {
                self.apply(SenderEvent::Error(e));
                None
            }
        }
    }
}

impl AudioHandler for Handler {
    fn audio_init(&self, format: AudioFormat) -> Box<dyn AudioSession> {
        Box::new(PcmSession {
            sink: self.open_sink(&format),
            state: self.state.clone(),
        })
    }

    fn on_volume(&self, volume: f32) {
        self.apply(SenderEvent::Volume(volume));
    }

    fn on_metadata(&self, metadata: &shairplay::TrackMetadata) {
        self.apply(SenderEvent::Metadata {
            title: metadata.title.clone(),
            artist: metadata.artist.clone(),
            album: metadata.album.clone(),
            duration_ms: metadata.duration_ms,
        });
    }

    fn on_progress(&self, start: u32, current: u32, end: u32) {
        self.apply(SenderEvent::Progress {
            start,
            current,
            end,
        });
    }

    fn on_remote_control(&self, remote: Arc<dyn shairplay::RemoteControl>) {
        self.apply(SenderEvent::RemoteAvailable(Arc::new(
            DacpSenderControl::new(remote),
        )));
    }

    fn on_client_connected(&self, addr: &str) {
        self.apply(SenderEvent::Connected {
            name: Some(addr.to_string()),
        });
    }

    fn on_client_disconnected(&self, addr: &str) {
        self.apply(SenderEvent::Disconnected {
            addr: addr.to_string(),
        });
    }

    fn on_error(&self, error: &shairplay::ShairplayError) {
        self.apply(SenderEvent::Error(error.to_string()));
    }
}

/// Per-stream PCM pump: applies the sender's volume and feeds the sink.
struct PcmSession {
    sink: Option<Sink>,
    state: Arc<Mutex<SessionState>>,
}

impl AudioSession for PcmSession {
    fn audio_process(&mut self, samples: &[f32]) {
        let Some(sink) = &mut self.sink else { return };
        let gain = self.state.lock().unwrap().volume_gain();
        if let Err(e) = sink.push(samples, gain) {
            // The sink is dead — report once, then go silent for the
            // rest of the stream instead of spamming the status row.
            self.sink = None;
            self.state
                .lock()
                .unwrap()
                .apply(SenderEvent::Error(e), Instant::now());
        }
    }

    fn audio_flush(&mut self) {
        self.state
            .lock()
            .unwrap()
            .apply(SenderEvent::Flush, Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Silent;
    impl AudioSession for Silent {
        fn audio_process(&mut self, _: &[f32]) {}
    }
    struct NoAudio;
    impl AudioHandler for NoAudio {
        fn audio_init(&self, _: AudioFormat) -> Box<dyn AudioSession> {
            Box::new(Silent)
        }
    }

    /// iPhones fail at play against an et=0-only advert (verified live).
    #[test]
    fn advert_offers_rsa_encryption() {
        let server = build_server("Test".into(), Arc::new(NoAudio)).expect("build");
        let txt = server.service_info().raop_txt;
        let et = txt.iter().find(|(k, _)| k == "et").map(|(_, v)| v.as_str());
        assert_eq!(et, Some("0,1"));
    }

    /// A random MAC per launch made the sender cache a new dead device
    /// every pane open — the address must be stable and well-formed.
    #[test]
    fn hwaddr_is_stable_locally_administered_unicast() {
        let a = hwaddr();
        assert_eq!(a, hwaddr());
        assert_eq!(a.len(), 6);
        assert_eq!(a[0] & 0x03, 0x02);
    }

    /// The `Receiver::drop` escape hatch for a player that stopped
    /// consuming: killing the stored pid unblocks the stdin write.
    #[cfg(unix)]
    #[test]
    fn kill_player_terminates_a_stuck_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        kill_player(child.id());
        assert!(!child.wait().expect("wait").success());
    }
}
