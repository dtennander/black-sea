use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use mmo_term::{GameEvent, Position, recv_event, send_event};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Context};
use ratatui::widgets::{Block, Paragraph};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const GRID_SIZE: f64 = 100.0;
const CURSOR_STEP: f32 = 1.0;
const BUBBLE_TTL: Duration = Duration::from_secs(5);
const BUBBLE_OFFSET: f32 = 3.0;
const OWN_BOAT: &str = "@";
const REMOTE_BOAT: &str = "^";

// ── App state ────────────────────────────────────────────────────────────────

struct Bubble {
    position: Position,
    text: String,
    received_at: Instant,
}

struct App {
    my_id: Option<u64>,
    cursor: Position,
    input: String,
    bubbles: Vec<Bubble>,
    remote_boats: HashMap<u64, Position>,
}

impl App {
    fn new() -> Self {
        Self {
            my_id: None,
            cursor: Position { x: 50.0, y: 50.0 },
            input: String::new(),
            bubbles: Vec::new(),
            remote_boats: HashMap::new(),
        }
    }

    fn move_cursor(&mut self, dx: f32, dy: f32) {
        self.cursor.x = (self.cursor.x + dx).clamp(0.0, 99.0);
        self.cursor.y = (self.cursor.y + dy).clamp(0.0, 99.0);
    }

    fn push_bubble(&mut self, position: Position, text: String) {
        self.bubbles.push(Bubble {
            position,
            text,
            received_at: Instant::now(),
        });
    }

    fn expire_bubbles(&mut self) {
        self.bubbles
            .retain(|b| b.received_at.elapsed() < BUBBLE_TTL);
    }
}

// ── Messages between tasks ───────────────────────────────────────────────────

enum AppMsg {
    Key(crossterm::event::KeyEvent),
    Tick,
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let server_url = std::env::var("MMO_SERVER")
        .unwrap_or_else(|_| "ws://127.0.0.1:7456".to_string());
    let request = server_url.into_client_request()?;
    let (mut ws, _) = connect_async(request).await?;

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut ws).await;
    ratatui::restore();
    result
}

async fn run(terminal: &mut ratatui::DefaultTerminal, ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>) -> Result<()> {
    let mut app = App::new();
    let (tx, mut rx) = mpsc::channel::<AppMsg>(64);

    // Spawn a task that reads keyboard / terminal events and forwards them.
    let tx_key = tx.clone();
    tokio::spawn(async move {
        loop {
            if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                if let Ok(Event::Key(key)) = event::read() {
                    if tx_key.send(AppMsg::Key(key)).await.is_err() {
                        break;
                    }
                }
            } else {
                if tx_key.send(AppMsg::Tick).await.is_err() {
                    break;
                }
            }
        }
    });

    loop {
        app.expire_bubbles();
        terminal.draw(|frame| render(frame, &app))?;

        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Some(AppMsg::Key(key)) => {
                        if key.kind != KeyEventKind::Press {
                            continue;
                        }
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('q') => break,
                            KeyCode::Up    => {
                                app.move_cursor(0.0, CURSOR_STEP);
                                send_move(ws, &app).await?;
                            }
                            KeyCode::Down  => {
                                app.move_cursor(0.0, -CURSOR_STEP);
                                send_move(ws, &app).await?;
                            }
                            KeyCode::Left  => {
                                app.move_cursor(-CURSOR_STEP, 0.0);
                                send_move(ws, &app).await?;
                            }
                            KeyCode::Right => {
                                app.move_cursor(CURSOR_STEP, 0.0);
                                send_move(ws, &app).await?;
                            }
                            KeyCode::Backspace => { app.input.pop(); }
                            KeyCode::Enter => {
                                if !app.input.is_empty() {
                                    let event = GameEvent::SayEvent {
                                        position: app.cursor.clone(),
                                        text: app.input.drain(..).collect(),
                                    };
                                    send_event(ws, &event).await?;
                                }
                            }
                            KeyCode::Char(c) => app.input.push(c),
                            _ => {}
                        }
                    }
                    Some(AppMsg::Tick) => {}
                    None => break,
                }
            }

            result = recv_event(ws) => {
                match result? {
                    Some(GameEvent::HelloEvent { your_id, start_position }) => {
                        app.my_id = Some(your_id);
                        app.cursor = start_position;
                    }
                    Some(GameEvent::WorldStateEvent { boats }) => {
                        for (id, position) in boats {
                            app.remote_boats.insert(id, position);
                        }
                    }
                    Some(GameEvent::MoveEvent { id, position }) => {
                        app.remote_boats.insert(id, position);
                    }
                    Some(GameEvent::ByeEvent { id }) => {
                        app.remote_boats.remove(&id);
                    }
                    Some(GameEvent::SayEvent { position, text }) => {
                        app.push_bubble(position, text);
                    }
                    None => break,
                }
            }
        }
    }

    Ok(())
}

/// Send a MoveEvent with the current cursor position.
async fn send_move(ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, app: &App) -> Result<()> {
    send_event(
        ws,
        &GameEvent::MoveEvent {
            id: app.my_id.unwrap_or(0),
            position: app.cursor.clone(),
        },
    )
    .await
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn render(frame: &mut Frame, app: &App) {
    let [world_area, input_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(frame.area());

    // ── Input bar ────────────────────────────────────────────────────────────
    let input_text = Line::from(vec![
        Span::styled("> ", Style::new().fg(Color::Yellow).bold()),
        Span::raw(&app.input),
    ]);
    let input_widget = Paragraph::new(input_text)
        .block(Block::bordered().title("Say (Enter to send, Esc to quit)"));
    frame.render_widget(input_widget, input_area);

    // ── World canvas ─────────────────────────────────────────────────────────
    let bubbles = &app.bubbles;
    let cursor = &app.cursor;
    let remote_boats = &app.remote_boats;

    let canvas = Canvas::default()
        .block(Block::bordered().title("World"))
        .x_bounds([0.0, GRID_SIZE])
        .y_bounds([0.0, GRID_SIZE])
        .paint(|ctx: &mut Context| {
            // Draw remote boats in cyan
            for position in remote_boats.values() {
                ctx.print(
                    position.x as f64,
                    position.y as f64,
                    Span::styled(REMOTE_BOAT, Style::new().fg(Color::Cyan)),
                );
            }

            // Draw own boat in yellow (on top)
            ctx.print(
                cursor.x as f64,
                cursor.y as f64,
                Span::styled(OWN_BOAT, Style::new().fg(Color::Yellow)),
            );

            // Draw speech bubbles
            for bubble in bubbles {
                let age = bubble.received_at.elapsed().as_secs_f64();
                let color = if age < BUBBLE_TTL.as_secs_f64() * 0.7 {
                    Color::White
                } else {
                    Color::DarkGray
                };
                let label = format!("[ {} ]", bubble.text);
                ctx.print(
                    bubble.position.x as f64,
                    bubble.position.y as f64 + BUBBLE_OFFSET as f64,
                    Span::styled(label, Style::new().fg(color)),
                );
            }
        });

    frame.render_widget(canvas, world_area);
}
