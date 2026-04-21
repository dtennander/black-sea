use anyhow::Result;
use black_sea_protocol::{GameEvent, send_event};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use std::time::Duration;

use crate::app::{App, CURSOR_STEP};

type ClientWs =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

// ── Incompatibility screen ────────────────────────────────────────────────────

/// Show a full-screen error indicating the client and server are incompatible.
/// Waits for the user to press Enter, Esc, or q before returning.
pub async fn show_incompatible_screen(
    terminal: &mut ratatui::DefaultTerminal,
    server_version: &str,
) -> Result<()> {
    loop {
        terminal.draw(|frame| render_incompatible_screen(frame, server_version))?;
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => return Ok(()),
                _ => {}
            }
        }
    }
}

fn render_incompatible_screen(frame: &mut Frame, server_version: &str) {
    let area = frame.area();
    let [_, center, _] = Layout::vertical([
        Constraint::Percentage(35),
        Constraint::Length(9),
        Constraint::Min(0),
    ])
    .areas(area);

    let own_version = env!("GIT_VERSION");
    let content = vec![
        Line::from(Span::styled(
            "  Client / server version mismatch",
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("  Server is running protocol v{server_version}"),
            Style::new().fg(Color::Yellow),
        )),
        Line::from(Span::styled(
            format!("  Your client is v{own_version}"),
            Style::new().fg(Color::Yellow),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  Run:  brew upgrade black-sea",
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  Press Enter or q to exit.",
            Style::new().fg(Color::DarkGray),
        )),
    ];

    let widget =
        Paragraph::new(content).block(Block::bordered().title("Update required — cannot connect"));
    frame.render_widget(widget, center);
}

// ── Name-entry screen ─────────────────────────────────────────────────────────

/// Show a name-entry TUI screen and return the name the user typed.
pub async fn prompt_name(terminal: &mut ratatui::DefaultTerminal) -> Result<String> {
    let mut name = String::new();
    loop {
        terminal.draw(|frame| render_name_screen(frame, &name))?;
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Enter if !name.is_empty() => return Ok(name),
                KeyCode::Backspace => {
                    name.pop();
                }
                KeyCode::Char(c) => {
                    if name.len() < 16 {
                        name.push(c);
                    }
                }
                _ => {}
            }
        }
    }
}

pub fn render_name_screen(frame: &mut Frame, name: &str) {
    let area = frame.area();
    let [_, center, _] = Layout::vertical([
        Constraint::Percentage(40),
        Constraint::Length(5),
        Constraint::Min(0),
    ])
    .areas(area);

    let preview = if name.is_empty() {
        "type your name…".to_string()
    } else {
        format!("/{}/", name)
    };

    let content = vec![
        Line::from(Span::styled(
            "Welcome to Black Sea",
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("> ", Style::new().fg(Color::Yellow).bold()),
            Span::raw(name),
            Span::styled("_", Style::new().fg(Color::DarkGray)),
        ]),
        Line::from(Span::styled(
            format!("  your boat: {}", preview),
            Style::new().fg(Color::DarkGray),
        )),
    ];

    let widget = Paragraph::new(content)
        .block(Block::bordered().title("Enter your boat name (Enter to sail)"));
    frame.render_widget(widget, center);
}

// ── In-game keyboard handling ─────────────────────────────────────────────────

/// Process a single key event during gameplay. Sends moves/chat to the server as needed.
pub async fn handle_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    ws: &mut ClientWs,
) -> Result<bool> {
    if key.kind != KeyEventKind::Press {
        return Ok(false);
    }

    // While the map overview is open, any key closes it.
    if app.show_map_overview {
        app.show_map_overview = false;
        return Ok(false);
    }

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            return Ok(true);
        }
        KeyCode::Up => {
            if app.move_cursor(0.0, -CURSOR_STEP) {
                send_move(ws, app).await?;
            }
        }
        KeyCode::Down => {
            if app.move_cursor(0.0, CURSOR_STEP) {
                send_move(ws, app).await?;
            }
        }
        KeyCode::Left => {
            if app.move_cursor(-CURSOR_STEP, 0.0) {
                send_move(ws, app).await?;
            }
        }
        KeyCode::Right => {
            if app.move_cursor(CURSOR_STEP, 0.0) {
                send_move(ws, app).await?;
            }
        }
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Enter => {
            if !app.input.is_empty() {
                let text: String = app.input.drain(..).collect();
                if text == "/map" {
                    app.show_map_overview = true;
                } else {
                    send_event(
                        ws,
                        &GameEvent::SayEvent {
                            position: None,
                            text: text.clone(),
                        },
                    )
                    .await?;
                    app.push_bubble(app.cursor.clone(), text);
                }
            }
        }
        KeyCode::Char(c) => app.input.push(c),
        _ => {}
    }
    Ok(false)
}

/// Helper: send the player's current position as a `MoveEvent`.
async fn send_move(ws: &mut ClientWs, app: &App) -> Result<()> {
    let Some(id) = app.my_id else {
        return Ok(());
    };
    send_event(
        ws,
        &GameEvent::MoveEvent {
            id,
            position: app.cursor.clone(),
        },
    )
    .await
}
