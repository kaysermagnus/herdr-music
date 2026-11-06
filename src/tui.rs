use crate::app::{Command, Controller, Snapshot};
use crate::provider::MusicProvider;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use std::io;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const FOOTER: &str = "space play/pause · s stop · n/p prev/next · ←/→ ±10s · r refresh · q quit";

enum WorkerMsg {
    Name(&'static str),
    State(Snapshot),
}

struct UiState {
    provider_name: &'static str,
    snapshot: Option<Snapshot>,
    received_at: Instant,
}

struct TermGuard;

impl TermGuard {
    fn enter() -> anyhow::Result<Self> {
        enable_raw_mode()?;
        if let Err(err) = execute!(io::stdout(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(err.into());
        }
        Ok(Self)
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// Run the TUI: this thread drives the terminal, a worker thread owns the
/// provider and controller, connected by two mpsc channels.
pub fn run(provider: Box<dyn MusicProvider>) -> anyhow::Result<()> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
    let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();

    let worker = thread::spawn(move || worker_loop(provider, cmd_rx, msg_tx));
    let _ = cmd_tx.send(Command::Refresh);

    let result = ui_loop(&msg_rx, &cmd_tx);

    let _ = cmd_tx.send(Command::Quit);
    let _ = worker.join();
    result
}

fn worker_loop(provider: Box<dyn MusicProvider>, rx: Receiver<Command>, tx: Sender<WorkerMsg>) {
    let mut controller = Controller::new(provider.name());
    let _ = tx.send(WorkerMsg::Name(provider.name()));
    loop {
        let cmd = match rx.recv_timeout(REFRESH_INTERVAL) {
            Ok(Command::Quit) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(cmd) => cmd,
            Err(RecvTimeoutError::Timeout) => Command::Refresh,
        };
        if controller.handle(cmd, &*provider)
            || tx.send(WorkerMsg::State(controller.snapshot())).is_err()
        {
            break;
        }
    }
}

fn ui_loop(rx: &Receiver<WorkerMsg>, tx: &Sender<Command>) -> anyhow::Result<()> {
    let _guard = TermGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut state = UiState {
        provider_name: "herdr-music",
        snapshot: None,
        received_at: Instant::now(),
    };
    loop {
        while let Ok(msg) = rx.try_recv() {
            match msg {
                WorkerMsg::Name(name) => state.provider_name = name,
                WorkerMsg::State(snap) => {
                    state.snapshot = Some(snap);
                    state.received_at = Instant::now();
                }
            }
        }
        terminal.draw(|frame| draw(frame, &state))?;
        if !event::poll(POLL_INTERVAL)? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match map_key(&key) {
                Some(Command::Quit) => break,
                Some(cmd) => {
                    let _ = tx.send(cmd);
                }
                None => {}
            }
        }
    }
    Ok(())
}

fn map_key(key: &KeyEvent) -> Option<Command> {
    match key.code {
        KeyCode::Char(' ') => Some(Command::Toggle),
        KeyCode::Char('s') => Some(Command::Stop),
        KeyCode::Char('n') => Some(Command::Next),
        KeyCode::Char('p') => Some(Command::Previous),
        KeyCode::Right | KeyCode::Char('l') => Some(Command::SeekForward),
        KeyCode::Left | KeyCode::Char('h') => Some(Command::SeekBack),
        KeyCode::Char('r') => Some(Command::Refresh),
        KeyCode::Char('q') | KeyCode::Esc => Some(Command::Quit),
        _ => None,
    }
}

fn draw(frame: &mut Frame, state: &UiState) {
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(frame.area());

    let status = match state.snapshot.as_ref().and_then(|s| s.status.as_deref()) {
        Some(msg) => Line::from(msg.to_string()).fg(Color::Red),
        None if state.snapshot.is_none() => Line::from("connecting…").dim(),
        None => Line::default(),
    };
    frame.render_widget(
        Paragraph::new(status).block(
            Block::default()
                .borders(Borders::ALL)
                .title(state.provider_name),
        ),
        chunks[0],
    );

    let playback = state.snapshot.as_ref().and_then(|s| s.playback.as_ref());
    let mut lines = Vec::new();
    match playback {
        Some(pb) => {
            lines.push(Line::from(pb.track.title.clone()).bold());
            lines.push(Line::from(pb.track.artists.join(", ")));
            lines.push(Line::from(pb.track.album.clone()));
            lines.push(Line::from(if pb.is_playing { "▶" } else { "❚❚" }));
            if let Some(device) = &pb.device {
                lines.push(Line::from(format!("on {device}")).dim());
            }
        }
        None => lines.push(Line::from("nothing playing").dim()),
    }
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("now playing")),
        chunks[1],
    );

    let (ratio, label) = match playback {
        Some(pb) => {
            let elapsed = pb.progress_at(state.received_at.elapsed());
            let total = pb.track.duration;
            let ratio = if total.is_zero() {
                0.0
            } else {
                (elapsed.as_secs_f64() / total.as_secs_f64()).clamp(0.0, 1.0)
            };
            (
                ratio,
                format!("{} / {}", fmt_duration(elapsed), fmt_duration(total)),
            )
        }
        None => (0.0, "--:-- / --:--".to_string()),
    };
    frame.render_widget(
        Gauge::default()
            .block(Block::default().borders(Borders::ALL))
            .ratio(ratio)
            .label(label),
        chunks[2],
    );

    frame.render_widget(
        Paragraph::new(FOOTER).style(Style::default().fg(Color::DarkGray)),
        chunks[3],
    );
}

pub(crate) fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Playback, Track};
    use crossterm::event::KeyModifiers;
    use ratatui::backend::TestBackend;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn playback() -> Playback {
        Playback {
            track: Track {
                title: "Song".to_string(),
                artists: vec!["Artist".to_string()],
                album: "Album".to_string(),
                duration: Duration::from_secs(200),
            },
            progress: Duration::from_secs(60),
            is_playing: true,
            device: Some("Mac".to_string()),
        }
    }

    fn state_with(snapshot: Option<Snapshot>) -> UiState {
        UiState {
            provider_name: "fake",
            snapshot,
            received_at: Instant::now(),
        }
    }

    fn rendered(state: &UiState) -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn map_key_covers_all_bindings() {
        let cases = [
            (KeyCode::Char(' '), Command::Toggle),
            (KeyCode::Char('s'), Command::Stop),
            (KeyCode::Char('n'), Command::Next),
            (KeyCode::Char('p'), Command::Previous),
            (KeyCode::Right, Command::SeekForward),
            (KeyCode::Char('l'), Command::SeekForward),
            (KeyCode::Left, Command::SeekBack),
            (KeyCode::Char('h'), Command::SeekBack),
            (KeyCode::Char('r'), Command::Refresh),
            (KeyCode::Char('q'), Command::Quit),
            (KeyCode::Esc, Command::Quit),
        ];
        for (code, expected) in cases {
            assert_eq!(map_key(&key(code)), Some(expected), "key {code:?}");
        }
        assert_eq!(map_key(&key(KeyCode::Char('x'))), None);
        assert_eq!(map_key(&key(KeyCode::Enter)), None);
    }

    #[test]
    fn fmt_duration_formats() {
        assert_eq!(fmt_duration(Duration::ZERO), "0:00");
        assert_eq!(fmt_duration(Duration::from_secs(7)), "0:07");
        assert_eq!(fmt_duration(Duration::from_secs(65)), "1:05");
        assert_eq!(fmt_duration(Duration::from_secs(3599)), "59:59");
        assert_eq!(fmt_duration(Duration::from_secs(3600)), "1:00:00");
        assert_eq!(fmt_duration(Duration::from_secs(3723)), "1:02:03");
    }

    #[test]
    fn draws_track_details() {
        let state = state_with(Some(Snapshot {
            playback: Some(playback()),
            status: None,
        }));
        let text = rendered(&state);
        assert!(text.contains("fake"));
        assert!(text.contains("Song"));
        assert!(text.contains("Artist"));
        assert!(text.contains("Album"));
        assert!(text.contains("Mac"));
        assert!(text.contains("3:20"));
    }

    #[test]
    fn draws_nothing_playing() {
        let state = state_with(Some(Snapshot {
            playback: None,
            status: None,
        }));
        assert!(rendered(&state).contains("nothing playing"));
    }

    #[test]
    fn draws_status_message() {
        let state = state_with(Some(Snapshot {
            playback: None,
            status: Some("not connected".to_string()),
        }));
        assert!(rendered(&state).contains("not connected"));
    }

    #[test]
    fn draws_connecting_before_first_snapshot() {
        assert!(rendered(&state_with(None)).contains("connecting"));
    }
}
