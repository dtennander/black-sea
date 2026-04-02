use std::collections::{HashMap, HashSet};
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

const CURSOR_STEP: f32 = 1.0;
const BUBBLE_TTL: Duration = Duration::from_secs(5);
const BUBBLE_OFFSET: f32 = 3.0;

/// How many tiles are visible in each axis on a single "page".
/// The actual on-screen size is mapped onto this tile window.
const PAGE_TILES_W: u32 = 200;
const PAGE_TILES_H: u32 = 50;

/// How many extra chunks to prefetch beyond the current page border (in chunk units).
const PREFETCH_BORDER: i32 = 1;

/// Tile values — must match server constants.
const TILE_WATER: u8 = 0;
const TILE_COAST: u8 = 1;
// TILE_LAND = 2 — not rendered but used for passability

/// Returns the ASCII character for a coastline tile at `(wx, wy)`.
///
/// Only cardinal neighbours are examined. Diagonals are ignored.
fn coast_char(app: &App, wx: u32, wy: u32) -> &'static str {
    let solid = |x: i32, y: i32| -> bool {
        if x < 0 || y < 0 { return false; }
        matches!(app.tile_at_world(x as u32, y as u32), Some(t) if t != TILE_WATER)
    };

    let x = wx as i32;
    let y = wy as i32;

    let n = solid(x,     y - 1);
    let s = solid(x,     y + 1);
    let w = solid(x - 1, y    );
    let e = solid(x + 1, y    );

    match (n, s, w, e) {
        (false, false, true,  true ) => "|",  // left/right wall
        (false, false, true,  false) => "|",
        (false, false, false, true ) => "|",
        (true,  true,  false, false) => "-",  // top/bottom wall
        (true,  false, false, false) => "-",
        (false, true,  false, false) => "-",
        (false, true,  false, true ) => "/",  // bottom-left corner
        (true,  false, true,  false) => "/",  // top-right corner
        (false, true,  true,  false) => "\\", // bottom-right corner
        (true,  false, false, true ) => "\\", // top-left corner
        _                            => " ",
    }
}

// ── Direction ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Default)]
enum Direction {
    Left,
    #[default]
    Right,
    Up,
    Down,
}

fn boat_glyphs(
    name: &str,
    dir: Direction,
    row_step: f64,
    col_step: f64,
) -> Vec<(f64, f64, String)> {
    match dir {
        Direction::Right => {
            let hull = format!("/{}/", name);
            let sail_x = ((hull.len() as f64 - 3.0) / 2.0).max(0.0) * col_step;
            vec![(0.0, 0.0, hull), (sail_x, row_step, "/|)".to_string())]
        }
        Direction::Left => {
            let hull = format!("\\{}\\", name);
            let sail_x = ((hull.len() as f64 - 3.0) / 2.0).max(0.0) * col_step;
            vec![(0.0, 0.0, hull), (sail_x, row_step, "(|\\".to_string())]
        }
        Direction::Up => vec![
            (col_step * 2.0, row_step * 2.0, "^".to_string()),
            (col_step, row_step, "/|\\".to_string()),
            (col_step, 0.0, "\\ /".to_string()),
        ],
        Direction::Down => vec![
            (col_step, row_step, "/ \\".to_string()),
            (col_step, 0., "\\|/".to_string()),
            (col_step * 2.0, -row_step, "V".to_string()),
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

/// Information received from the server's WorldInfoEvent.
struct WorldInfo {
    tile_width: u32,
    tile_height: u32,
    chunk_size: u32,
}

struct App {
    my_id: Option<u64>,
    my_name: String,
    /// Player position in tile coordinates.
    cursor: Position,
    last_dir: Direction,
    input: String,
    bubbles: Vec<Bubble>,
    remote_boats: HashMap<u64, RemoteBoat>,

    // ── Map state ─────────────────────────────────────────────────────────────
    world_info: Option<WorldInfo>,
    /// Loaded chunk data, keyed by (chunk_x, chunk_y).
    loaded_chunks: HashMap<(u32, u32), Vec<u8>>,
    /// Chunks that have been requested but not yet received.
    pending_chunks: HashSet<(u32, u32)>,
    /// Outbound chunk requests buffered here so the game loop can send them.
    chunk_requests: Vec<(u32, u32)>,
}

impl App {
    fn new(name: String) -> Self {
        Self {
            my_id: None,
            my_name: name,
            cursor: Position { x: 250.0, y: 250.0 }, // centre of the 500×500 map
            last_dir: Direction::Right,
            input: String::new(),
            bubbles: Vec::new(),
            remote_boats: HashMap::new(),
            world_info: None,
            loaded_chunks: HashMap::new(),
            pending_chunks: HashSet::new(),
            chunk_requests: Vec::new(),
        }
    }

    // ── Page/viewport helpers ─────────────────────────────────────────────────

    /// Which page the cursor is on (page coordinates).
    fn current_page(&self) -> (u32, u32) {
        (
            (self.cursor.x as u32) / PAGE_TILES_W,
            (self.cursor.y as u32) / PAGE_TILES_H,
        )
    }

    /// Tile origin (top-left) of the current page in world-tile coordinates.
    fn page_origin(&self) -> (u32, u32) {
        let (px, py) = self.current_page();
        (px * PAGE_TILES_W, py * PAGE_TILES_H)
    }

    // ── Chunk management ──────────────────────────────────────────────────────

    /// Queue chunk requests for the current page plus a 1-chunk prefetch border.
    /// Should be called after any move or after WorldInfoEvent is received.
    fn ensure_chunks_loaded(&mut self) {
        let wi = match &self.world_info {
            Some(wi) => (wi.tile_width, wi.tile_height, wi.chunk_size),
            None => return,
        };
        let (tile_w, tile_h, cs) = wi;
        let chunks_x = tile_w.div_ceil(cs);
        let chunks_y = tile_h.div_ceil(cs);

        // Convert cursor tile position to chunk coordinates.
        let cur_cx = (self.cursor.x as u32) / cs;
        let cur_cy = (self.cursor.y as u32) / cs;

        // Additionally cover the current page in full (it may span >1 chunk).
        let (origin_x, origin_y) = self.page_origin();
        let page_cx_min = origin_x / cs;
        let page_cx_max = (origin_x + PAGE_TILES_W - 1) / cs;
        let page_cy_min = origin_y / cs;
        let page_cy_max = (origin_y + PAGE_TILES_H - 1) / cs;

        // Range to request: page extent + PREFETCH_BORDER on each side.
        let cx_min = (page_cx_min as i32 - PREFETCH_BORDER).max(0) as u32;
        let cx_max =
            ((page_cx_max as i32 + PREFETCH_BORDER) as u32).min(chunks_x.saturating_sub(1));
        let cy_min = (page_cy_min as i32 - PREFETCH_BORDER).max(0) as u32;
        let cy_max =
            ((page_cy_max as i32 + PREFETCH_BORDER) as u32).min(chunks_y.saturating_sub(1));

        // Also ensure the chunk directly under the cursor is included.
        let _ = cur_cx; // already covered by page extent in most cases
        let _ = cur_cy;

        for cy in cy_min..=cy_max {
            for cx in cx_min..=cx_max {
                let key = (cx, cy);
                if !self.loaded_chunks.contains_key(&key) && !self.pending_chunks.contains(&key) {
                    self.pending_chunks.insert(key);
                    self.chunk_requests.push(key);
                }
            }
        }
    }

    // ── Movement ──────────────────────────────────────────────────────────────

    /// Try to move the cursor by (dx, dy).  Returns true if the move was accepted.
    /// Blocks moves onto TILE_COAST or TILE_LAND if local chunk data is available.
    fn move_cursor(&mut self, dx: f32, dy: f32) -> bool {
        let max_x = self
            .world_info
            .as_ref()
            .map(|w| w.tile_width as f32 - 1.0)
            .unwrap_or(499.0);
        let max_y = self
            .world_info
            .as_ref()
            .map(|w| w.tile_height as f32 - 1.0)
            .unwrap_or(499.0);

        let new_x = (self.cursor.x + dx).clamp(0.0, max_x);
        let new_y = (self.cursor.y + dy).clamp(0.0, max_y);
        let candidate = Position { x: new_x, y: new_y };

        // Local passability check (only if we have the data).
        if let Some(wi) = &self.world_info {
            let cs = wi.chunk_size;
            let cx = (new_x as u32) / cs;
            let cy = (new_y as u32) / cs;
            if let Some(chunk) = self.loaded_chunks.get(&(cx, cy)) {
                let local_col = (new_x as u32) % cs;
                let local_row = (new_y as u32) % cs;
                let idx = (local_row * cs + local_col) as usize;
                let tile = chunk.get(idx).copied().unwrap_or(TILE_WATER);
                if tile != TILE_WATER {
                    return false; // blocked locally — don't send to server
                }
            }
        }

        let new_dir = match (dx, dy) {
            (x, _) if x > 0.0 => Direction::Right,
            (x, _) if x < 0.0 => Direction::Left,
            (_, y) if y < 0.0 => Direction::Up,
            _ => Direction::Down,
        };

        let old_page = self.current_page();
        self.cursor = candidate;
        self.last_dir = new_dir;

        // If we crossed a page boundary, prefetch new chunks.
        if self.current_page() != old_page {
            self.ensure_chunks_loaded();
        }

        true
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

    // ── Tile lookup ───────────────────────────────────────────────────────────

    fn tile_at_world(&self, wx: u32, wy: u32) -> Option<u8> {
        let wi = self.world_info.as_ref()?;
        let cs = wi.chunk_size;
        let cx = wx / cs;
        let cy = wy / cs;
        let chunk = self.loaded_chunks.get(&(cx, cy))?;
        let local_col = wx % cs;
        let local_row = wy % cs;
        let idx = (local_row * cs + local_col) as usize;
        chunk.get(idx).copied()
    }
}

// ── Messages between tasks ────────────────────────────────────────────────────

enum AppMsg {
    Key(crossterm::event::KeyEvent),
    Tick,
}

// ── Name-entry screen ─────────────────────────────────────────────────────────

async fn prompt_name(terminal: &mut ratatui::DefaultTerminal) -> Result<String> {
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

    let name = match prompt_name(&mut terminal).await {
        Ok(n) => n,
        Err(e) => {
            ratatui::restore();
            return Err(e);
        }
    };

    let request = server_url.into_client_request()?;
    let (mut ws, _) = connect_async(request).await?;

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

    let tx_key = tx.clone();
    tokio::spawn(async move {
        loop {
            if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                if let Ok(Event::Key(key)) = event::read()
                    && tx_key.send(AppMsg::Key(key)).await.is_err()
                {
                    break;
                }
            } else if tx_key.send(AppMsg::Tick).await.is_err() {
                break;
            }
        }
    });

    loop {
        app.expire_bubbles();
        terminal.draw(|frame| render(frame, &app))?;

        // Drain any pending chunk requests.
        for (cx, cy) in app.chunk_requests.drain(..).collect::<Vec<_>>() {
            send_event(
                ws,
                &GameEvent::MapChunkRequest {
                    chunk_x: cx,
                    chunk_y: cy,
                },
            )
            .await?;
        }

        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Some(AppMsg::Key(key)) => {
                        if key.kind != KeyEventKind::Press {
                            continue;
                        }
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('q') => break,
                            KeyCode::Up => {
                                if app.move_cursor(0.0, -CURSOR_STEP) {
                                    send_move(ws, &app).await?;
                                }
                            }
                            KeyCode::Down => {
                                if app.move_cursor(0.0, CURSOR_STEP) {
                                    send_move(ws, &app).await?;
                                }
                            }
                            KeyCode::Left => {
                                if app.move_cursor(-CURSOR_STEP, 0.0) {
                                    send_move(ws, &app).await?;
                                }
                            }
                            KeyCode::Right => {
                                if app.move_cursor(CURSOR_STEP, 0.0) {
                                    send_move(ws, &app).await?;
                                }
                            }
                            KeyCode::Backspace => { app.input.pop(); }
                            KeyCode::Enter => {
                                if !app.input.is_empty() {
                                    let text: String = app.input.drain(..).collect();
                                    send_event(ws, &GameEvent::SayEvent {
                                        position: None,
                                        text: text.clone(),
                                    }).await?;
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

                    Some(GameEvent::WorldInfoEvent { tile_width, tile_height, chunk_size, .. }) => {
                        app.world_info = Some(WorldInfo { tile_width, tile_height, chunk_size });
                        app.ensure_chunks_loaded();
                    }

                    Some(GameEvent::MapChunkResponse { chunk_x, chunk_y, data }) => {
                        app.pending_chunks.remove(&(chunk_x, chunk_y));
                        app.loaded_chunks.insert((chunk_x, chunk_y), data);
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
                        app.remote_boats
                            .entry(id)
                            .and_modify(|b| b.name = name.clone())
                            .or_insert(RemoteBoat {
                                position: Position { x: 0.0, y: 0.0 },
                                name,
                                last_dir: Direction::Right,
                            });
                    }
                    Some(GameEvent::MoveEvent { id, position }) => {
                        if let Some(boat) = app.remote_boats.get_mut(&id) {
                            let dx = position.x - boat.position.x;
                            let dy = position.y - boat.position.y;
                            if dx.abs() > 0.0 || dy.abs() > 0.0 {
                                boat.last_dir = if dx.abs() >= dy.abs() {
                                    if dx > 0.0 { Direction::Right } else { Direction::Left }
                                } else {
                                    if dy > 0.0 { Direction::Down } else { Direction::Up }
                                };
                            }
                            boat.position = position;
                        } else {
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
                    // Client should never receive these
                    Some(GameEvent::RegisterEvent { .. })
                    | Some(GameEvent::MapChunkRequest { .. }) => {}
                    None => break,
                }
            }
        }
    }

    Ok(())
}

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
    let (page_ox, page_oy) = app.page_origin();

    // Canvas logical bounds map 1:1 onto page tiles (0..PAGE_TILES_W, 0..PAGE_TILES_H).
    // Y axis: ratatui canvas has Y increasing upward, so row 0 = bottom of screen.
    // We store tiles with row 0 = northernmost (top), so we flip: canvas_y = PAGE_TILES_H - 1 - tile_row_within_page.
    let canvas_w = PAGE_TILES_W as f64;
    let canvas_h = PAGE_TILES_H as f64;

    let inner_h = (world_area.height.saturating_sub(2)) as f64;
    let inner_w = (world_area.width.saturating_sub(2)) as f64;
    let row_step = if inner_h > 1.0 {
        canvas_h / (inner_h - 1.0)
    } else {
        1.0
    };
    let col_step = if inner_w > 1.0 {
        canvas_w / (inner_w - 1.0)
    } else {
        1.0
    };

    let v_margin = row_step * 3.0;
    let clamp_y = |y: f64, dir: Direction| -> f64 {
        match dir {
            Direction::Up | Direction::Down => y.clamp(0.0, canvas_h - v_margin),
            _ => y,
        }
    };

    // Convert a world-tile position to canvas coordinates.
    let world_to_canvas = |wx: f32, wy: f32| -> (f64, f64) {
        let cx = (wx as f64) - (page_ox as f64);
        // Flip Y: tile row increases downward, canvas Y increases upward.
        let cy = canvas_h - 1.0 - ((wy as f64) - (page_oy as f64));
        (cx, cy)
    };

    let (own_cx, own_cy_raw) = world_to_canvas(app.cursor.x, app.cursor.y);
    let own_cy = clamp_y(own_cy_raw, app.last_dir);
    let own_glyphs = boat_glyphs(&app.my_name, app.last_dir, row_step, col_step);

    let (page_ox_cap, page_oy_cap) = (page_ox, page_oy);

    // Build page title showing coordinates.
    let (ppx, ppy) = app.current_page();
    let title = format!(
        "World  [{}, {}]  page ({}, {})",
        app.cursor.x as u32, app.cursor.y as u32, ppx, ppy
    );

    let canvas = Canvas::default()
        .block(Block::bordered().title(title))
        .x_bounds([0.0, canvas_w])
        .y_bounds([0.0, canvas_h])
        .paint(move |ctx: &mut Context| {
            // ── Draw coastline tiles ─────────────────────────────────────────
            // Iterate over tiles visible on this page.
            for tile_y in page_oy_cap..page_oy_cap + PAGE_TILES_H {
                for tile_x in page_ox_cap..page_ox_cap + PAGE_TILES_W {
                    if let Some(TILE_COAST) = app.tile_at_world(tile_x, tile_y) {
                        // Skip isolated coast tiles with no solid cardinal neighbour —
                        // these are tiny islets that read as noise at this zoom level.
                        let has_solid_neighbour = [
                            (tile_x.wrapping_sub(1), tile_y),
                            (tile_x + 1,             tile_y),
                            (tile_x,                 tile_y.wrapping_sub(1)),
                            (tile_x,                 tile_y + 1),
                        ]
                        .iter()
                        .any(|&(nx, ny)| matches!(
                            app.tile_at_world(nx, ny),
                            Some(t) if t != TILE_WATER
                        ));
                        if !has_solid_neighbour { continue; }

                        let cx = (tile_x - page_ox_cap) as f64;
                        // Flip Y for canvas
                        let cy = canvas_h - 1.0 - (tile_y - page_oy_cap) as f64;
                        let ch = coast_char(app, tile_x, tile_y);
                        ctx.print(cx, cy, Span::styled(ch, Style::new().fg(Color::Green)));
                    }
                }
            }

            // ── Remote boats ─────────────────────────────────────────────────
            for boat in app.remote_boats.values() {
                let (bx, by_raw) = world_to_canvas(boat.position.x, boat.position.y);
                let by = clamp_y(by_raw, boat.last_dir);
                for (x_off, y_off, glyph) in
                    boat_glyphs(&boat.name, boat.last_dir, row_step, col_step)
                {
                    ctx.print(
                        bx + x_off,
                        by + y_off,
                        Span::styled(glyph, Style::new().fg(Color::Cyan)),
                    );
                }
            }

            // ── Own boat ─────────────────────────────────────────────────────
            for (x_off, y_off, glyph) in &own_glyphs {
                ctx.print(
                    own_cx + x_off,
                    own_cy + y_off,
                    Span::styled(
                        glyph.clone(),
                        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                );
            }

            // ── Speech bubbles ───────────────────────────────────────────────
            for bubble in &app.bubbles {
                let (bx, by) = world_to_canvas(bubble.position.x, bubble.position.y);
                let age = bubble.received_at.elapsed().as_secs_f64();
                let color = if age < BUBBLE_TTL.as_secs_f64() * 0.7 {
                    Color::White
                } else {
                    Color::DarkGray
                };
                ctx.print(
                    bx,
                    by + BUBBLE_OFFSET as f64,
                    Span::styled(format!("[ {} ]", bubble.text), Style::new().fg(color)),
                );
            }
        });

    frame.render_widget(canvas, world_area);
}
