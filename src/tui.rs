use crate::app::{Command, Controller, Snapshot};
use crate::provider::MusicProvider;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use std::io;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const FOOTER: &str = "space play/pause   n/p skip   ←/→ 10s   q quit";
const EQ_LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
const EQ_STEP_MS: u128 = 250;

enum WorkerMsg {
    Name(&'static str),
    State(Snapshot),
}

struct UiState {
    provider_name: &'static str,
    snapshot: Option<Snapshot>,
    received_at: Instant,
    started_at: Instant,
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
        started_at: Instant::now(),
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

/// One accent per provider — its own brand color, not decoration.
fn accent_for(provider: &str) -> Color {
    match provider {
        "spotify" => Color::Green,
        _ => Color::Cyan,
    }
}

/// Four-bar level meter: moves while playing, rests while paused, flat
/// when nothing is loaded. Deterministic from wall time, no extra state.
fn eq_bars(playing: Option<bool>, t: Duration) -> String {
    match playing {
        Some(true) => {
            let step = (t.as_millis() / EQ_STEP_MS) as u64 ^ 0x5BD1_E995;
            let h = step.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            (0..4)
                .map(|i| EQ_LEVELS[1 + ((h >> (i * 8)) % 7) as usize])
                .collect()
        }
        Some(false) => "▂▃▂▁".into(),
        None => "▁▁▁▁".into(),
    }
}

/// Scrub bar of exactly `width` cells: played track, playhead, remaining.
fn scrubber(width: usize, ratio: f64, accent: Color) -> Vec<Span<'static>> {
    let width = width.max(3);
    let played = ((width - 1) as f64 * ratio.clamp(0.0, 1.0)).round() as usize;
    vec![
        Span::styled("━".repeat(played), Style::default().fg(accent)),
        Span::styled("●", Style::default().fg(accent).bold()),
        Span::styled("─".repeat(width - 1 - played), Style::default().dim()),
    ]
}

fn draw(frame: &mut Frame, state: &UiState) {
    let accent = accent_for(state.provider_name);
    let dim = Style::default().dim();

    let card = Block::default()
        .borders(Borders::ALL)
        .border_style(dim)
        .title(Span::styled(
            format!(" {} ", state.provider_name),
            Style::default().fg(accent),
        ));
    let inner = card.inner(frame.area());
    frame.render_widget(card, frame.area());
    let inner = Rect {
        x: inner.x + 2,
        width: inner.width.saturating_sub(4),
        ..inner
    };
    let rows = Layout::vertical([
        Constraint::Length(1), // breathing room
        Constraint::Length(1), // meter + title
        Constraint::Length(1), // artist
        Constraint::Length(1), // album
        Constraint::Length(1),
        Constraint::Length(1), // scrubber
        Constraint::Length(1),
        Constraint::Length(1), // transport
        Constraint::Min(0),
        Constraint::Length(1), // device / status
        Constraint::Length(1), // keys
    ])
    .split(inner);

    let snapshot = state.snapshot.as_ref();
    let playback = snapshot.and_then(|s| s.playback.as_ref());
    let playing = playback.map(|pb| pb.is_playing);
    let status = snapshot.and_then(|s| s.status.as_deref());

    let (title, artist, album) = match playback {
        Some(pb) => (
            pb.track.title.clone(),
            pb.track.artists.join(", "),
            pb.track.album.clone(),
        ),
        // Blank while connecting or when an error already explains the state.
        None if snapshot.is_none() || status.is_some() => Default::default(),
        None => (
            "Nothing playing".to_string(),
            "Start a song on any device and it shows up here".to_string(),
            String::new(),
        ),
    };
    let title_style = if playback.is_some() {
        Style::default().bold()
    } else {
        dim
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                eq_bars(playing, state.started_at.elapsed()),
                Style::default().fg(accent),
            ),
            Span::raw("  "),
            Span::styled(title, title_style),
        ])),
        rows[1],
    );
    frame.render_widget(Paragraph::new(format!("      {artist}")), rows[2]);
    frame.render_widget(Paragraph::new(format!("      {album}")).style(dim), rows[3]);

    let (elapsed, total) = match playback {
        Some(pb) => (
            pb.progress_at(state.received_at.elapsed()),
            pb.track.duration,
        ),
        None => (Duration::ZERO, Duration::ZERO),
    };
    let ratio = if total.is_zero() {
        0.0
    } else {
        elapsed.as_secs_f64() / total.as_secs_f64()
    };
    let (left, right) = if playback.is_some() {
        (fmt_duration(elapsed), fmt_duration(total))
    } else {
        ("-:--".to_string(), "-:--".to_string())
    };
    let bar_width = (rows[5].width as usize).saturating_sub(left.len() + right.len() + 4);
    let bar_accent = if playback.is_some() {
        accent
    } else {
        Color::DarkGray
    };
    let mut track = vec![Span::styled(left, dim), Span::raw("  ")];
    track.extend(scrubber(bar_width, ratio, bar_accent));
    track.push(Span::raw("  "));
    track.push(Span::styled(right, dim));
    frame.render_widget(Paragraph::new(Line::from(track)), rows[5]);

    let action = if playing == Some(true) {
        "❚❚"
    } else {
        "▶ "
    };
    frame.render_widget(
        Paragraph::new(
            Line::from(vec![
                Span::styled("◀◀", dim),
                Span::raw("     "),
                Span::styled(action, Style::default().fg(accent).bold()),
                Span::raw("     "),
                Span::styled("▶▶", dim),
            ])
            .centered(),
        ),
        rows[7],
    );

    let bottom = match status {
        Some(err) => Line::from(err.to_string()).fg(Color::Red),
        None if snapshot.is_none() => {
            Line::from(format!("Connecting to {}…", state.provider_name)).style(dim)
        }
        None => match playback.and_then(|pb| pb.device.as_deref()) {
            Some(device) => Line::from(format!("on {device}")).style(dim),
            None => Line::default(),
        },
    };
    frame.render_widget(Paragraph::new(bottom), rows[9]);
    frame.render_widget(
        Paragraph::new(FOOTER).style(Style::default().fg(Color::DarkGray)),
        rows[10],
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
            started_at: Instant::now(),
        }
    }

    fn rendered(state: &UiState) -> String {
        let mut terminal = Terminal::new(TestBackend::new(60, 14)).unwrap();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn spans_text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn scrubber_is_exact_width_with_playhead_at_ratio() {
        let start = spans_text(&scrubber(20, 0.0, Color::Green));
        assert_eq!(start.chars().count(), 20);
        assert!(start.starts_with('●'));

        let end = spans_text(&scrubber(20, 1.0, Color::Green));
        assert_eq!(end.chars().count(), 20);
        assert!(end.ends_with('●'));

        let half = spans_text(&scrubber(21, 0.5, Color::Green));
        assert_eq!(half.chars().nth(10), Some('●'));
    }

    #[test]
    fn eq_bars_reflect_state() {
        assert_eq!(eq_bars(None, Duration::ZERO), "▁▁▁▁");
        assert_eq!(eq_bars(Some(false), Duration::ZERO), "▂▃▂▁");
        let a = eq_bars(Some(true), Duration::from_millis(0));
        let b = eq_bars(Some(true), Duration::from_millis(EQ_STEP_MS as u64 * 3));
        assert_eq!(a.chars().count(), 4);
        assert!(a.chars().all(|c| EQ_LEVELS.contains(&c)));
        assert_ne!(a, b);
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
        assert!(text.contains("on Mac"));
        assert!(text.contains("1:00"));
        assert!(text.contains("3:20"));
        assert!(text.contains("❚❚"), "playing shows the pause action");
        assert!(text.contains('●'));
    }

    #[test]
    fn paused_shows_play_action() {
        let mut pb = playback();
        pb.is_playing = false;
        let state = state_with(Some(Snapshot {
            playback: Some(pb),
            status: None,
        }));
        let text = rendered(&state);
        assert!(text.contains("◀◀     ▶"));
        assert!(!text.contains("❚❚"));
    }

    #[test]
    fn draws_nothing_playing() {
        let state = state_with(Some(Snapshot {
            playback: None,
            status: None,
        }));
        let text = rendered(&state);
        assert!(text.contains("Nothing playing"));
        assert!(text.contains("-:--"));
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
        assert!(rendered(&state_with(None)).contains("Connecting to fake"));
    }

    #[test]
    fn error_replaces_empty_state_hint() {
        let state = state_with(Some(Snapshot {
            playback: None,
            status: Some("not connected".to_string()),
        }));
        let text = rendered(&state);
        assert!(text.contains("not connected"));
        assert!(!text.contains("Nothing playing"));
    }

    #[test]
    fn survives_tiny_pane() {
        let state = state_with(Some(Snapshot {
            playback: Some(playback()),
            status: None,
        }));
        let mut terminal = Terminal::new(TestBackend::new(12, 4)).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
    }
}
