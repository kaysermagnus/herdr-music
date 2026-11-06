use crate::app::{picker_rows, Command, Controller, PickerAction, Snapshot};
use crate::provider::{LocalStatus, MusicProvider, Playback};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    MouseButton, MouseEventKind,
};
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
const FOOTER: &str = "space play/pause   n/p skip   ←/→ 10s   d devices   q quit";
const PICKER_FOOTER: &str = "↑/↓ choose   enter select   d/esc close";
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
        if let Err(err) = execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture) {
            let _ = disable_raw_mode();
            return Err(err.into());
        }
        Ok(Self)
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
}

/// Run the TUI: this thread drives the terminal, a worker thread owns the
/// provider and controller, connected by two mpsc channels. `local_name`
/// is the local engine's device name, when local playback is enabled.
pub fn run(provider: Box<dyn MusicProvider>, local_name: Option<String>) -> anyhow::Result<()> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
    let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();

    let worker = thread::spawn(move || worker_loop(provider, local_name, cmd_rx, msg_tx));
    let _ = cmd_tx.send(Command::Refresh);
    let _ = cmd_tx.send(Command::StartLocal);

    let result = ui_loop(&msg_rx, &cmd_tx);

    let _ = cmd_tx.send(Command::Quit);
    let _ = worker.join();
    result
}

fn worker_loop(
    provider: Box<dyn MusicProvider>,
    local_name: Option<String>,
    rx: Receiver<Command>,
    tx: Sender<WorkerMsg>,
) {
    let mut controller = Controller::new().with_local(local_name);
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
        let cmd = match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                map_key(&key, state.snapshot.as_ref())
            }
            Event::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left) => {
                let size = terminal.size()?;
                let playback = state.snapshot.as_ref().and_then(|s| s.playback.as_ref());
                let r = regions(
                    Rect::new(0, 0, size.width, size.height),
                    time_width(playback),
                );
                click(&r, state.snapshot.as_ref(), m.column, m.row)
            }
            _ => None,
        };
        match cmd {
            Some(Command::Quit) => break,
            Some(cmd) => {
                let _ = tx.send(cmd);
            }
            None => {}
        }
    }
    Ok(())
}

fn map_key(key: &KeyEvent, snap: Option<&Snapshot>) -> Option<Command> {
    let picker_open = snap.map(|s| s.picker_open).unwrap_or(false);
    match key.code {
        KeyCode::Char(' ') => Some(Command::Toggle),
        KeyCode::Char('s') => Some(Command::Stop),
        KeyCode::Char('n') => Some(Command::Next),
        KeyCode::Char('p') => Some(Command::Previous),
        KeyCode::Right | KeyCode::Char('l') if !picker_open => Some(Command::SeekForward),
        KeyCode::Left | KeyCode::Char('h') if !picker_open => Some(Command::SeekBack),
        KeyCode::Char('r') => Some(Command::Refresh),
        KeyCode::Char('d') => Some(Command::Devices),
        KeyCode::Up | KeyCode::Char('k') if picker_open => Some(Command::PickerMove(-1)),
        KeyCode::Down | KeyCode::Char('j') if picker_open => Some(Command::PickerMove(1)),
        KeyCode::Enter if picker_open => Some(Command::PickerSelect),
        KeyCode::Char('q') => Some(Command::Quit),
        // Esc closes the picker if it's open, otherwise quits.
        KeyCode::Esc if picker_open => Some(Command::Devices),
        KeyCode::Esc => Some(Command::Quit),
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

/// Screen areas shared by `draw` and `click`, so a click always lands on
/// what was drawn there.
struct Regions {
    title: Rect,
    artist: Rect,
    album: Rect,
    elapsed: Rect,
    bar: Rect,
    total: Rect,
    prev: Rect,
    toggle: Rect,
    next: Rect,
    status: Rect,
    keys: Rect,
    /// The device picker's overlay, spanning the track rows to the footer.
    picker: Rect,
}

fn regions(area: Rect, time_width: u16) -> Regions {
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let inner = Rect {
        x: inner.x + 2,
        width: inner.width.saturating_sub(4),
        ..inner
    };
    let [_, title, artist, album, _, scrub, _, transport, _, status, keys] = Layout::vertical([
        Constraint::Length(1), // breathing room
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let [elapsed, _, bar, _, total] = Layout::horizontal([
        Constraint::Length(time_width),
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(2),
        Constraint::Length(time_width),
    ])
    .areas(scrub);
    let [_, prev, _, toggle, _, next, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(2),
        Constraint::Length(5),
        Constraint::Length(2),
        Constraint::Length(5),
        Constraint::Length(2),
        Constraint::Fill(1),
    ])
    .areas(transport);
    Regions {
        title,
        artist,
        album,
        elapsed,
        bar,
        total,
        prev,
        toggle,
        next,
        status,
        keys,
        picker: Rect {
            x: inner.x,
            y: title.y,
            width: inner.width,
            height: keys.y + keys.height - title.y,
        },
    }
}

/// The picker's row area (inside its border), shared by draw and click.
fn picker_inner(picker: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(picker)
}

/// Width of the longest time label: the track length, since elapsed never
/// exceeds it (`-:--` when idle is 4 cells too).
fn time_width(playback: Option<&Playback>) -> u16 {
    playback.map_or(4, |pb| fmt_duration(pb.track.duration).len() as u16)
}

/// Map a left click to a command. Buttons get one cell of slack on each
/// side; a click on the scrub bar seeks to that spot. While the picker is
/// open, clicks land on its rows instead.
fn click(r: &Regions, snap: Option<&Snapshot>, col: u16, row: u16) -> Option<Command> {
    let empty;
    let snap = match snap {
        Some(s) => s,
        // No snapshot yet (pre-connect): buttons should still map.
        None => {
            empty = Snapshot::default();
            &empty
        }
    };
    if snap.picker_open {
        let inner = picker_inner(r.picker);
        if col < inner.x || col >= inner.x + inner.width || row < inner.y {
            return None;
        }
        let index = (row - inner.y) as usize;
        let rows = picker_rows(
            &snap.devices,
            snap.local_name.as_deref(),
            &snap.local_status,
        );
        return match rows.get(index).map(|r| &r.action) {
            Some(PickerAction::Transfer(id)) => Some(Command::SelectDevice(id.clone())),
            Some(PickerAction::StartLocal) => Some(Command::StartLocal),
            _ => None,
        };
    }
    let playback = snap.playback.as_ref();
    let hit = |rect: Rect, slack: u16| {
        rect.width > 0
            && row >= rect.y
            && row < rect.y + rect.height
            && col + slack >= rect.x
            && col < rect.x + rect.width + slack
    };
    if hit(r.prev, 1) {
        Some(Command::Previous)
    } else if hit(r.toggle, 1) {
        Some(Command::Toggle)
    } else if hit(r.next, 1) {
        Some(Command::Next)
    } else if hit(r.bar, 0) {
        let pb = playback?;
        let span = r.bar.width.saturating_sub(1).max(1) as f64;
        let ratio = ((col - r.bar.x) as f64 / span).min(1.0);
        Some(Command::SeekTo(pb.track.duration.mul_f64(ratio)))
    } else {
        None
    }
}

fn draw(frame: &mut Frame, state: &UiState) {
    let accent = accent_for(state.provider_name);
    let dim = Style::default().dim();

    let snapshot = state.snapshot.as_ref();
    let playback = snapshot.and_then(|s| s.playback.as_ref());
    let playing = playback.map(|pb| pb.is_playing);
    let status = snapshot.and_then(|s| s.status.as_deref());
    let r = regions(frame.area(), time_width(playback));

    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(dim)
            .title(Span::styled(
                format!(" {} ", state.provider_name),
                Style::default().fg(accent),
            )),
        frame.area(),
    );

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
        r.title,
    );
    frame.render_widget(Paragraph::new(format!("      {artist}")), r.artist);
    frame.render_widget(Paragraph::new(format!("      {album}")).style(dim), r.album);

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
    let bar_accent = if playback.is_some() {
        accent
    } else {
        Color::DarkGray
    };
    frame.render_widget(Paragraph::new(left).style(dim).right_aligned(), r.elapsed);
    frame.render_widget(
        Paragraph::new(Line::from(scrubber(
            r.bar.width as usize,
            ratio,
            bar_accent,
        ))),
        r.bar,
    );
    frame.render_widget(Paragraph::new(right).style(dim), r.total);

    let action = if playing == Some(true) {
        "❚❚"
    } else {
        "▶ "
    };
    frame.render_widget(Paragraph::new("◀◀").style(dim), r.prev);
    frame.render_widget(
        Paragraph::new(action).style(Style::default().fg(accent).bold()),
        r.toggle,
    );
    frame.render_widget(Paragraph::new("▶▶").style(dim), r.next);

    let bottom = match status {
        Some(err) => Line::from(err.to_string()).fg(Color::Red),
        None if snapshot.is_none() => {
            Line::from(format!("Connecting to {}…", state.provider_name)).style(dim)
        }
        None => match snapshot.and_then(|s| s.local_alert.as_deref()) {
            Some(alert) => {
                let style = match snapshot.map(|s| &s.local_status) {
                    // "starting…" is progress, not an error.
                    Some(LocalStatus::Starting) => dim,
                    _ => Style::default().fg(Color::Red),
                };
                Line::from(alert.to_string()).style(style)
            }
            None => match playback.and_then(|pb| pb.device.as_deref()) {
                Some(device) => Line::from(format!("on {device}")).style(dim),
                None => Line::default(),
            },
        },
    };
    frame.render_widget(Paragraph::new(bottom), r.status);

    let picker_open = snapshot.map(|s| s.picker_open).unwrap_or(false);
    if picker_open {
        draw_picker(frame, r.picker, snapshot.unwrap(), accent, dim);
    }
    frame.render_widget(
        Paragraph::new(if picker_open { PICKER_FOOTER } else { FOOTER })
            .style(Style::default().fg(Color::DarkGray)),
        r.keys,
    );
}

/// The device picker: a bordered list overlaid on the card. Active device
/// carries the accent glyph; the highlighted row is prefixed.
fn draw_picker(frame: &mut Frame, area: Rect, snap: &Snapshot, accent: Color, dim: Style) {
    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(dim)
            .title(Span::styled(" devices ", Style::default().fg(accent))),
        area,
    );
    let inner = picker_inner(area);
    let rows = picker_rows(
        &snap.devices,
        snap.local_name.as_deref(),
        &snap.local_status,
    );
    for (i, row) in rows.iter().enumerate().take(inner.height as usize) {
        let marker = if i == snap.picker_index {
            "▸"
        } else if row.active {
            "●"
        } else {
            " "
        };
        let style = if i == snap.picker_index {
            Style::default().fg(accent).bold()
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(marker, Style::default().fg(accent)),
                Span::styled(format!(" {}", row.label), style),
            ])),
            Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
        );
    }
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
    use crate::provider::{Device, Playback, Track};
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

    fn device(id: &str, name: &str, active: bool) -> Device {
        Device {
            id: id.into(),
            name: name.into(),
            is_active: active,
        }
    }

    /// A snapshot with just playback filled — the shape tests used before
    /// the picker fields existed.
    fn snap(playback: Option<Playback>) -> Snapshot {
        Snapshot {
            playback,
            status: None,
            devices: Vec::new(),
            local_name: None,
            local_status: LocalStatus::NotStarted,
            local_alert: None,
            picker_open: false,
            picker_index: 0,
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

    const AREA: Rect = Rect::new(0, 0, 60, 14);

    fn render_buf(state: &UiState) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(AREA.width, AREA.height)).unwrap();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn rendered(state: &UiState) -> String {
        let buf = render_buf(state);
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
            (KeyCode::Char('d'), Command::Devices),
            (KeyCode::Char('q'), Command::Quit),
            (KeyCode::Esc, Command::Quit),
        ];
        for (code, expected) in cases {
            assert_eq!(map_key(&key(code), None), Some(expected), "key {code:?}");
        }
        assert_eq!(map_key(&key(KeyCode::Char('x')), None), None);
        assert_eq!(map_key(&key(KeyCode::Enter), None), None);
    }

    #[test]
    fn map_key_picker_bindings() {
        let mut open = snap(None);
        open.picker_open = true;
        let cases = [
            (KeyCode::Up, Command::PickerMove(-1)),
            (KeyCode::Char('k'), Command::PickerMove(-1)),
            (KeyCode::Down, Command::PickerMove(1)),
            (KeyCode::Char('j'), Command::PickerMove(1)),
            (KeyCode::Enter, Command::PickerSelect),
            (KeyCode::Esc, Command::Devices),
        ];
        for (code, expected) in cases {
            assert_eq!(
                map_key(&key(code), Some(&open)),
                Some(expected),
                "key {code:?}"
            );
        }
        // Seek keys are swallowed while the picker is open; quit works.
        assert_eq!(map_key(&key(KeyCode::Right), Some(&open)), None);
        assert_eq!(
            map_key(&key(KeyCode::Char('q')), Some(&open)),
            Some(Command::Quit)
        );
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
            ..snap(None)
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
        let state = state_with(Some(snap(Some(pb))));
        let text = rendered(&state);
        assert!(text.contains("◀◀     ▶"));
        assert!(!text.contains("❚❚"));
    }

    #[test]
    fn draws_nothing_playing() {
        let state = state_with(Some(snap(None)));
        let text = rendered(&state);
        assert!(text.contains("Nothing playing"));
        assert!(text.contains("-:--"));
    }

    #[test]
    fn draws_status_message() {
        let state = state_with(Some(Snapshot {
            status: Some("not connected".to_string()),
            ..snap(None)
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
            status: Some("not connected".to_string()),
            ..snap(None)
        }));
        let text = rendered(&state);
        assert!(text.contains("not connected"));
        assert!(!text.contains("Nothing playing"));
    }

    #[test]
    fn clicks_on_buttons_map_to_transport() {
        let pb = playback();
        let r = regions(AREA, time_width(Some(&pb)));
        let s = snap(Some(pb));
        let click_at = |rect: Rect| click(&r, Some(&s), rect.x, rect.y);
        assert_eq!(click_at(r.prev), Some(Command::Previous));
        assert_eq!(click_at(r.toggle), Some(Command::Toggle));
        assert_eq!(click_at(r.next), Some(Command::Next));
        // One cell of slack either side of a button.
        assert_eq!(
            click(&r, Some(&s), r.toggle.x - 1, r.toggle.y),
            Some(Command::Toggle)
        );
        // The middle of the gap between buttons does nothing.
        assert_eq!(click(&r, Some(&s), r.prev.x + 4, r.prev.y), None);
        assert_eq!(click_at(r.title), None);
    }

    #[test]
    fn click_on_bar_seeks_to_that_spot() {
        let pb = playback(); // 200s track
        let r = regions(AREA, time_width(Some(&pb)));
        let s = snap(Some(pb));
        let at = |col| click(&r, Some(&s), col, r.bar.y);
        assert_eq!(at(r.bar.x), Some(Command::SeekTo(Duration::ZERO)));
        assert_eq!(
            at(r.bar.x + r.bar.width - 1),
            Some(Command::SeekTo(Duration::from_secs(200)))
        );
        match at(r.bar.x + (r.bar.width - 1) / 2) {
            Some(Command::SeekTo(d)) => assert!((95..=105).contains(&d.as_secs()), "{d:?}"),
            other => panic!("expected seek, got {other:?}"),
        }
        assert_eq!(click(&r, None, r.bar.x, r.bar.y), None, "nothing to seek");
    }

    #[test]
    fn picker_rows_are_drawn_where_clicks_land() {
        let mut s = snap(None);
        s.picker_open = true;
        s.local_name = Some("Herdr Music".into());
        s.devices = vec![
            device("d1", "Kitchen", true),
            device("d2", "Herdr Music", false),
        ];
        let r = regions(AREA, 4);
        let buf = render_buf(&state_with(Some(s.clone())));
        let inner = picker_inner(r.picker);

        // Row 0 is Kitchen (active), row 1 is the local device.
        assert!(buf[(inner.x + 2, inner.y)].symbol() == "K");
        assert_eq!(buf[(inner.x, inner.y + 1)].symbol(), " ");
        assert_eq!(
            click(&r, Some(&s), inner.x, inner.y),
            Some(Command::SelectDevice("d1".into()))
        );
        assert_eq!(
            click(&r, Some(&s), inner.x, inner.y + 1),
            Some(Command::SelectDevice("d2".into()))
        );
        // Below the last row, nothing maps.
        assert_eq!(click(&r, Some(&s), inner.x, inner.y + 2), None);
    }

    #[test]
    fn picker_renders_labels_and_footer() {
        let mut s = snap(None);
        s.picker_open = true;
        s.local_name = Some("Herdr Music".into());
        s.local_status = LocalStatus::Failed("dead".into());
        s.devices = vec![device("d1", "Kitchen", true)];
        let text = rendered(&state_with(Some(s)));
        assert!(text.contains("Kitchen"));
        assert!(text.contains("Herdr Music (this laptop — restart)"));
        assert!(text.contains("enter select"));
    }

    #[test]
    fn engine_status_line_shows_alert_and_starting() {
        let mut s = snap(None);
        s.local_status = LocalStatus::Starting;
        s.local_alert = Some("starting local playback…".into());
        assert!(rendered(&state_with(Some(s))).contains("starting local playback"));

        let mut s = snap(None);
        s.local_status = LocalStatus::Failed("engine went boom".into());
        s.local_alert = Some("engine went boom".into());
        assert!(rendered(&state_with(Some(s))).contains("engine went boom"));
    }

    #[test]
    fn buttons_are_drawn_where_clicks_land() {
        let pb = playback();
        let r = regions(AREA, time_width(Some(&pb)));
        let buf = render_buf(&state_with(Some(snap(Some(pb)))));
        assert_eq!(buf[(r.prev.x, r.prev.y)].symbol(), "◀");
        assert_eq!(buf[(r.toggle.x, r.toggle.y)].symbol(), "❚");
        assert_eq!(buf[(r.next.x, r.next.y)].symbol(), "▶");
        assert!(["━", "●"].contains(&buf[(r.bar.x, r.bar.y)].symbol()));
        assert_eq!(buf[(r.bar.x + r.bar.width - 1, r.bar.y)].symbol(), "─");
    }

    #[test]
    fn time_column_fits_the_track_length() {
        let mut pb = playback();
        assert_eq!(time_width(None), 4);
        pb.track.duration = Duration::from_secs(200);
        assert_eq!(time_width(Some(&pb)), 4);
        pb.track.duration = Duration::from_secs(1200);
        assert_eq!(time_width(Some(&pb)), 5);
        pb.track.duration = Duration::from_secs(3723);
        assert_eq!(time_width(Some(&pb)), 7);
    }

    #[test]
    fn survives_tiny_pane_with_picker_open() {
        let mut s = snap(Some(playback()));
        s.picker_open = true;
        s.devices = vec![device("d1", "Kitchen", true)];
        let state = state_with(Some(s));
        let mut terminal = Terminal::new(TestBackend::new(12, 4)).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
    }

    #[test]
    fn survives_tiny_pane() {
        let state = state_with(Some(snap(Some(playback()))));
        let mut terminal = Terminal::new(TestBackend::new(12, 4)).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
    }
}
