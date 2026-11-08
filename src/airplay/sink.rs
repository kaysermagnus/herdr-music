//! PCM output. macOS/Windows use `cpal` against the OS default output;
//! Linux pipes s16le to the first of `pw-play`/`paplay`/`aplay` on PATH —
//! no audio headers needed to build.
//!
//! The pure parts (gain, byte conversion, Linux player choice and argv)
//! are platform-agnostic so they are testable on any dev machine.

/// Status the TUI shows when Linux has no usable player binary.
pub const NO_PLAYER_MSG: &str =
    "no audio player found — install pipewire-utils, pulseaudio-utils or alsa-utils";

/// Sender volume is dB in −30…0, −144 is mute. Linear gain: `10^(dB/20)`.
pub fn db_to_gain(db: f32) -> f32 {
    if db <= -144.0 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// Interleaved f32 → s16le bytes with `gain` applied, clipped to ±1.
pub fn s16le_bytes(samples: &[f32], gain: f32) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let v = (s * gain).clamp(-1.0, 1.0);
        out.extend_from_slice(&((v * i16::MAX as f32) as i16).to_le_bytes());
    }
    out
}

/// PipeWire/PulseAudio/ALSA command-line players, in preference order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipePlayer {
    PwPlay,
    Paplay,
    Aplay,
}

const PLAYERS: [(PipePlayer, &str); 3] = [
    (PipePlayer::PwPlay, "pw-play"),
    (PipePlayer::Paplay, "paplay"),
    (PipePlayer::Aplay, "aplay"),
];

/// First usable player from `available` (a "which"-style name list).
pub fn choose_player(available: &[&str]) -> Option<PipePlayer> {
    PLAYERS
        .iter()
        .find(|(_, bin)| available.contains(bin))
        .map(|(player, _)| *player)
}

/// Raw-s16le argv for `player` at the stream's rate/channels.
pub fn player_argv(player: PipePlayer, rate: u32, channels: u8) -> Vec<String> {
    let (rate, channels) = (rate.to_string(), channels.to_string());
    match player {
        PipePlayer::PwPlay => vec![
            "pw-play".into(),
            "--format=s16".into(),
            format!("--rate={rate}"),
            format!("--channels={channels}"),
            "-".into(),
        ],
        PipePlayer::Paplay => vec![
            "paplay".into(),
            "--raw".into(),
            "--format=s16le".into(),
            format!("--rate={rate}"),
            format!("--channels={channels}"),
            "-".into(),
        ],
        PipePlayer::Aplay => vec![
            "aplay".into(),
            "-t".into(),
            "raw".into(),
            "-f".into(),
            "S16_LE".into(),
            "-r".into(),
            rate,
            "-c".into(),
            channels,
            "-".into(),
        ],
    }
}

/// The player binaries found on PATH, in preference order.
#[cfg(target_os = "linux")]
fn players_on_path() -> Vec<&'static str> {
    let on_path = |name: &str| {
        std::env::var_os("PATH")
            .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
    };
    PLAYERS
        .iter()
        .map(|(_, bin)| *bin)
        .filter(|bin| on_path(bin))
        .collect()
}

/// First usable player on PATH, if any.
#[cfg(target_os = "linux")]
pub fn detect_player() -> Option<PipePlayer> {
    choose_player(&players_on_path())
}

/// What `Sink::open` needs to pick an output: the detected player binary
/// on Linux, nothing on cpal platforms.
#[cfg(target_os = "linux")]
pub type PlayerChoice = Option<PipePlayer>;
#[cfg(not(target_os = "linux"))]
pub type PlayerChoice = ();

/// A child process player fed s16le on stdin (Linux).
#[cfg(target_os = "linux")]
pub struct Sink {
    child: std::process::Child,
    /// Shared slot for the child's pid, so `Receiver::drop` can kill a
    /// player that stopped consuming (its full stdin pipe would
    /// otherwise block `push` forever and hang shutdown).
    sink_pid: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

#[cfg(target_os = "linux")]
impl Sink {
    /// Spawn `player` reading raw PCM from stdin; `Err` message on failure.
    /// `state` reports stream errors on the cpal path — unused on Linux,
    /// where failures return `Err` instead. `sink_pid` records the
    /// child's pid for `Receiver::drop`.
    pub fn open(
        player: &PlayerChoice,
        format: &shairplay::AudioFormat,
        _state: std::sync::Arc<std::sync::Mutex<crate::airplay::state::SessionState>>,
        sink_pid: std::sync::Arc<std::sync::atomic::AtomicU32>,
    ) -> Result<Self, String> {
        let player = (*player).ok_or_else(|| NO_PLAYER_MSG.to_string())?;
        use std::process::{Command, Stdio};
        let argv = player_argv(player, format.sample_rate, format.channels);
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to start {}: {e}", argv[0]))?;
        if child.stdin.is_none() {
            return Err(format!("{} did not accept a stdin pipe", argv[0]));
        }
        sink_pid.store(child.id(), std::sync::atomic::Ordering::Relaxed);
        Ok(Self { child, sink_pid })
    }

    /// Gain-adjusted PCM to the player's stdin. A `BrokenPipe` means the
    /// player exited; surface it once via the error string.
    pub fn push(&mut self, samples: &[f32], gain: f32) -> Result<(), String> {
        use std::io::Write;
        let bytes = s16le_bytes(samples, gain);
        self.child
            .stdin
            .as_mut()
            .expect("stdin piped at open")
            .write_all(&bytes)
            .map_err(|e| format!("audio player pipe: {e}"))
    }
}

#[cfg(target_os = "linux")]
impl Drop for Sink {
    fn drop(&mut self) {
        let id = self.child.id();
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Release the pid only if it is still ours — a newer sink may
        // have already stored its child.
        let _ = self.sink_pid.compare_exchange(
            id,
            0,
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

/// cpal output against the OS default device (macOS CoreAudio, Windows
/// WASAPI). Samples queue up gained-and-ready for the device callback.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub struct Sink {
    _stream: cpal::Stream,
    queue: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<f32>>>,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl Sink {
    /// Bounded queue: ~2 s of stereo 44.1 kHz. Overflow drops the oldest
    /// samples rather than letting latency grow unbounded.
    const QUEUE_CAP: usize = 44_100 * 2 * 2;

    /// Open the default output device with an f32 stream at the
    /// incoming format's rate/channels. `player` is the Linux-only
    /// choice, unused here; `sink_pid` is its shutdown escape hatch,
    /// also unused. `state` carries stream errors to the status row —
    /// writing to the terminal would corrupt the TUI.
    pub fn open(
        _player: &PlayerChoice,
        format: &shairplay::AudioFormat,
        state: std::sync::Arc<std::sync::Mutex<crate::airplay::state::SessionState>>,
        _sink_pid: std::sync::Arc<std::sync::atomic::AtomicU32>,
    ) -> Result<Self, String> {
        use cpal::traits::{DeviceTrait, HostTrait};
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no default audio output device".to_string())?;
        let config = device
            .default_output_config()
            .map_err(|e| format!("no default output config: {e}"))?;
        if config.sample_format() != cpal::SampleFormat::F32 {
            return Err(format!(
                "default audio output does not take f32 samples ({:?})",
                config.sample_format()
            ));
        }
        let mut stream_config = config.config();
        stream_config.sample_rate = format.sample_rate;
        stream_config.channels = format.channels as cpal::ChannelCount;

        let queue = std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::VecDeque::with_capacity(Self::QUEUE_CAP),
        ));
        let feed = queue.clone();
        let errors = state;
        let stream = device
            .build_output_stream(
                stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let mut feed = feed.lock().unwrap();
                    for slot in data.iter_mut() {
                        *slot = feed.pop_front().unwrap_or(0.0);
                    }
                },
                move |e| {
                    errors.lock().unwrap().apply(
                        crate::airplay::state::SenderEvent::Error(format!("audio output: {e}")),
                        std::time::Instant::now(),
                    );
                },
                None,
            )
            .map_err(|e| format!("failed to open audio output: {e}"))?;
        use cpal::traits::StreamTrait;
        stream
            .play()
            .map_err(|e| format!("failed to start audio output: {e}"))?;
        Ok(Self {
            _stream: stream,
            queue,
        })
    }

    /// Queue gained samples for the output callback.
    pub fn push(&mut self, samples: &[f32], gain: f32) -> Result<(), String> {
        let mut queue = self.queue.lock().unwrap();
        let keep = Self::QUEUE_CAP.saturating_sub(samples.len());
        let excess = queue.len().saturating_sub(keep);
        if excess > 0 {
            queue.drain(..excess);
        }
        queue.extend(samples.iter().map(|s| s * gain));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_to_gain_maps_sender_volume() {
        assert_eq!(db_to_gain(0.0), 1.0);
        assert!((db_to_gain(-30.0) - 0.0316).abs() < 0.0001);
        assert_eq!(db_to_gain(-144.0), 0.0);
        assert_eq!(db_to_gain(-200.0), 0.0);
    }

    #[test]
    fn s16le_bytes_applies_gain_and_clips() {
        let bytes = s16le_bytes(&[0.5, -0.5, 2.0, -2.0], 1.0);
        let samples: Vec<i16> = bytes
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(samples[0], i16::MAX / 2);
        assert_eq!(samples[1], -(i16::MAX / 2));
        assert_eq!(samples[2], i16::MAX);
        assert_eq!(samples[3], -i16::MAX);

        let muted = s16le_bytes(&[1.0], 0.0);
        assert_eq!(muted, [0, 0]);
    }

    #[test]
    fn player_argv_matches_each_binary() {
        let argv = player_argv(PipePlayer::PwPlay, 44_100, 2);
        assert_eq!(argv[0], "pw-play");
        assert!(argv.contains(&"--format=s16".to_string()));
        assert!(argv.contains(&"--rate=44100".to_string()));
        assert!(argv.contains(&"--channels=2".to_string()));
        assert_eq!(argv.last(), Some(&"-".to_string()));

        let argv = player_argv(PipePlayer::Paplay, 44_100, 2);
        assert_eq!(argv[0], "paplay");
        assert!(argv.contains(&"--raw".to_string()));
        assert!(argv.contains(&"--format=s16le".to_string()));
        assert_eq!(argv.last(), Some(&"-".to_string()));

        let argv = player_argv(PipePlayer::Aplay, 48_000, 2);
        assert_eq!(argv[0], "aplay");
        assert!(argv.contains(&"raw".to_string()));
        assert!(argv.contains(&"S16_LE".to_string()));
        assert!(argv.contains(&"48000".to_string()));
        assert_eq!(argv.last(), Some(&"-".to_string()));
    }

    #[test]
    fn choose_player_prefers_pipewire_then_pulse_then_alsa() {
        assert_eq!(
            choose_player(&["pw-play", "paplay", "aplay"]),
            Some(PipePlayer::PwPlay)
        );
        assert_eq!(
            choose_player(&["aplay", "paplay"]),
            Some(PipePlayer::Paplay)
        );
        assert_eq!(choose_player(&["aplay"]), Some(PipePlayer::Aplay));
        assert_eq!(choose_player(&["mpv", "vlc"]), None);
        assert_eq!(choose_player(&[]), None);
    }

    #[test]
    fn no_player_message_names_the_packages() {
        assert!(NO_PLAYER_MSG.contains("pipewire-utils"));
        assert!(NO_PLAYER_MSG.contains("pulseaudio-utils"));
        assert!(NO_PLAYER_MSG.contains("alsa-utils"));
    }
}
