use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use black_sea::{GameEvent, Position, recv_event, send_event};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
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

// ── Direction ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Default for Direction {
    fn default() -> Self {
        Direction::Right
    }
}

/// Build the boat glyph string from a name and the last movement direction.
/// Horizontal: /Name/ or \Name\  — name visible on the broadside.
/// Vertical: ^ or v              — name hidden, bow/stern facing viewer.
/// Returns a list of `(x_offset, y_offset, text)` tuples to print relative to the boat's position.
/// Horizontal boats have a sail row above the hull; vertical boats stack three rows.
/// `row_step` is the number of logical units that equals exactly one terminal row, so that
/// multi-row boats always land on distinct cells regardless of terminal height.
fn boat_glyphs(name: &str, dir: Direction, row_step: f64) -> Vec<(f64, f64, String)> {
    match dir {
        Direction::Right => {
            let hull = format!("/{}/", name);
            // Sail centered over hull: hull is name.len()+2 wide, sail "\|)" is 3 wide.
            let sail_x = ((hull.len() as f64 - 3.0) / 2.0).max(0.0);
            vec![(0.0, 0.0, hull), (sail_x, row_step, "/|)".to_string())]
        }
        Direction::Left => {
            let hull = format!("\\{}\\", name);
            let sail_x = ((hull.len() as f64 - 3.0) / 2.0).max(0.0);
            vec![(0.0, 0.0, hull), (sail_x, row_step, "(|\\".to_string())]
        }
        Direction::Up => vec![
            (0.0, 0.0, "||".to_string()),
            (0.0, row_step, "||".to_string()),
            (0.0, row_step * 2.0, "/\\".to_string()),
        ],
        Direction::Down => vec![
            (0.0, 0.0, "\\/".to_string()),
            (0.0, row_step, "||".to_string()),
            (0.0, row_step * 2.0, "||".to_string()),
        ],
    }
}

// ── App state ─────────────────────────────────────────────────────────────────

struct Bubble {
    position: Position,
    text: String,
    received_at: Instant,
}

struct RemoteBoat {
    position: Position,
    name: String,
    last_dir: Direction,
}

struct App {
    my_id: Option<u64>,
    my_name: String,
    cursor: Position,
    last_dir: Direction,
    input: String,
    bubbles: Vec<Bubble>,
    remote_boats: HashMap<u64, RemoteBoat>,
}

impl App {
    fn new(name: String) -> Self {
        Self {
            my_id: None,
            my_name: name,
            cursor: Position { x: 50.0, y: 50.0 },
            last_dir: Direction::Right,
            input: String::new(),
            bubbles: Vec::new(),
            remote_boats: HashMap::new(),
        }
    }

    fn move_cursor(&mut self, dx: f32, dy: f32) {
        self.cursor.x = (self.cursor.x + dx).clamp(0.0, 99.0);
        self.cursor.y = (self.cursor.y + dy).clamp(0.0, 99.0);
        self.last_dir = match (dx, dy) {
            (x, _) if x > 0.0 => Direction::Right,
            (x, _) if x < 0.0 => Direction::Left,
            (_, y) if y > 0.0 => Direction::Up,
            _ => Direction::Down,
        };
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

// ── Name-entry screen ─────────────────────────────────────────────────────────

/// Show a simple TUI name-entry screen and return the name the player types.
async fn prompt_name(terminal: &mut ratatui::DefaultTerminal) -> Result<String> {
    let mut name = String::new();
    loop {
        terminal.draw(|frame| render_name_screen(frame, &name))?;

        // Block until a key event (poll with short timeout for responsiveness)
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Enter if !name.is_empty() => return Ok(name),
                    KeyCode::Backspace => {
                        name.pop();
                    }
                    KeyCode::Char(c) => {
                        // Cap name length to keep glyphs readable
                        if name.len() < 16 {
                            name.push(c);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn render_name_screen(frame: &mut Frame, name: &str) {
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

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let server_url = std::env::var("BLACK_SEA_SERVER").unwrap_or_else(|_| {
        option_env!("BLACK_SEA_SERVER_DEFAULT")
            .unwrap_or("ws://127.0.0.1:7456")
            .to_string()
    });

    let mut terminal = ratatui::init();

    // Ask for a name before connecting.
    let name = match prompt_name(&mut terminal).await {
        Ok(n) => n,
        Err(e) => {
            ratatui::restore();
            return Err(e);
        }
    };

    let request = server_url.into_client_request()?;
    let (mut ws, _) = connect_async(request).await?;

    // Register name with server immediately.
    send_event(&mut ws, &GameEvent::RegisterEvent { name: name.clone() }).await?;

    let result = run(&mut terminal, &mut ws, name).await;
    ratatui::restore();
    result
}

async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    name: String,
) -> Result<()> {
    let mut app = App::new(name);
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
                                    let text: String = app.input.drain(..).collect();
                                    let event = GameEvent::SayEvent {
                                        position: None,
                                        text: text.clone(),
                                    };
                                    send_event(ws, &event).await?;
                                    // Show the bubble locally — the server won't echo it back to us.
                                    app.push_bubble(app.cursor.clone(), text);
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
                        for (id, position, name) in boats {
                            app.remote_boats.insert(id, RemoteBoat {
                                position,
                                name,
                                last_dir: Direction::Right,
                            });
                        }
                    }
                    Some(GameEvent::NameEvent { id, name }) => {
                        // A new player just connected — add them with a default position
                        // (a MoveEvent will follow with their real position shortly, but
                        // we want their name ready).
                        app.remote_boats.entry(id).and_modify(|b| b.name = name.clone()).or_insert(RemoteBoat {
                            position: Position { x: 0.0, y: 0.0 },
                            name,
                            last_dir: Direction::Right,
                        });
                    }
                    Some(GameEvent::MoveEvent { id, position }) => {
                        if let Some(boat) = app.remote_boats.get_mut(&id) {
                            // Derive direction from position delta before updating
                            let dx = position.x - boat.position.x;
                            let dy = position.y - boat.position.y;
                            if dx.abs() > 0.0 || dy.abs() > 0.0 {
                                boat.last_dir = if dx.abs() >= dy.abs() {
                                    if dx > 0.0 { Direction::Right } else { Direction::Left }
                                } else {
                                    if dy > 0.0 { Direction::Up } else { Direction::Down }
                                };
                            }
                            boat.position = position;
                        } else {
                            // Boat not yet in map (NameEvent may not have arrived yet)
                            app.remote_boats.insert(id, RemoteBoat {
                                position,
                                name: id.to_string(),
                                last_dir: Direction::Right,
                            });
                        }
                    }
                    Some(GameEvent::ByeEvent { id }) => {
                        app.remote_boats.remove(&id);
                    }
                    Some(GameEvent::SayEvent { position, text }) => {
                        app.push_bubble(position.unwrap_or_else(|| app.cursor.clone()), text);
                    }
                    // Server should never send these to a client
                    Some(GameEvent::RegisterEvent { .. }) => {}
                    None => break,
                }
            }
        }
    }

    Ok(())
}

/// Send a MoveEvent with the current cursor position.
async fn send_move(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    app: &App,
) -> Result<()> {
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

    // Compute how many logical units correspond to exactly one terminal row/col.
    // The canvas inner area subtracts 2 for the border on each axis.
    let inner_h = (world_area.height.saturating_sub(2)) as f64;
    let row_step = if inner_h > 1.0 {
        GRID_SIZE / (inner_h - 1.0)
    } else {
        1.0
    };

    // For vertical boats (3 rows tall), clamp the base y so all rows stay in-bounds.
    let v_margin = row_step * 2.0;
    let clamp_y = |y: f32, dir: Direction| -> f64 {
        match dir {
            Direction::Up | Direction::Down => (y as f64).clamp(0.0, GRID_SIZE - v_margin),
            _ => y as f64,
        }
    };

    let own_glyphs = boat_glyphs(&app.my_name, app.last_dir, row_step);

    let canvas = Canvas::default()
        .block(Block::bordered().title("World"))
        .x_bounds([0.0, GRID_SIZE])
        .y_bounds([0.0, GRID_SIZE])
        .paint(|ctx: &mut Context| {
            // Draw remote boats in cyan — name embedded in the hull glyph
            for boat in remote_boats.values() {
                let base_y = clamp_y(boat.position.y, boat.last_dir);
                for (x_off, y_off, glyph) in boat_glyphs(&boat.name, boat.last_dir, row_step) {
                    ctx.print(
                        boat.position.x as f64 + x_off,
                        base_y + y_off,
                        Span::styled(glyph, Style::new().fg(Color::Cyan)),
                    );
                }
            }

            // Draw own boat in yellow (on top)
            let own_base_y = clamp_y(cursor.y, app.last_dir);
            for (x_off, y_off, glyph) in &own_glyphs {
                ctx.print(
                    cursor.x as f64 + x_off,
                    own_base_y + y_off,
                    Span::styled(
                        glyph.clone(),
                        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                );
            }

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
