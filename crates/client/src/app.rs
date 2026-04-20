use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::Result;
use black_sea_protocol::{GameEvent, Position, Tile, recv_event, send_event};
use crossterm::event::KeyEvent;
use semver::Version;
use tokio::sync::mpsc;

use crate::render::render;

// ── Constants ─────────────────────────────────────────────────────────────────

pub const CURSOR_STEP: f32 = 1.0;
pub const BUBBLE_TTL: Duration = Duration::from_secs(5);
pub const BUBBLE_OFFSET: f32 = 3.0;

/// Number of tiles visible in each axis per "page" (viewport).
pub const PAGE_TILES_W: u32 = 200;
pub const PAGE_TILES_H: u32 = 50;

/// Extra chunks to prefetch beyond the current page border (in chunk units).
pub const PREFETCH_BORDER: i32 = 1;

// ── Direction ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Direction {
    Left,
    #[default]
    Right,
    Up,
    Down,
}

// ── Domain types ──────────────────────────────────────────────────────────────

pub struct Bubble {
    pub position: Position,
    pub text: String,
    pub received_at: Instant,
}

pub struct RemoteBoat {
    pub position: Position,
    pub name: String,
    pub last_dir: Direction,
}

/// Map metadata received from the server's `WorldInfoEvent`.
pub struct WorldInfo {
    pub tile_width: u32,
    pub tile_height: u32,
    pub chunk_size: u32,
}

/// Low-resolution overview of the entire map, received from the server.
pub struct OverviewMap {
    pub width: u32,
    pub height: u32,
    pub data: Vec<Tile>,
}

// ── Update notification state ─────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub enum UpdateStatus {
    /// No version information received yet.
    #[default]
    Unknown,
    /// Server version matches major.minor; a newer patch may be available.
    Compatible { patch_available: Option<String> },
    /// Server major or minor version differs — client needs to update.
    Incompatible { server_version: String },
}

// ── Messages between the input task and the game loop ────────────────────────

pub enum AppMsg {
    Key(KeyEvent),
    Tick,
    LatestRelease(String),
}

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    pub my_id: Option<u64>,
    pub my_name: String,
    /// Player position in tile coordinates.
    pub cursor: Position,
    pub last_dir: Direction,
    pub input: String,
    pub bubbles: Vec<Bubble>,
    pub remote_boats: HashMap<u64, RemoteBoat>,

    // ── Map state ─────────────────────────────────────────────────────────────
    pub world_info: Option<WorldInfo>,
    /// Loaded chunk data, keyed by `(chunk_x, chunk_y)`.
    pub loaded_chunks: HashMap<(u32, u32), Vec<Tile>>,
    /// Chunks that have been requested but not yet received.
    pub pending_chunks: HashSet<(u32, u32)>,
    /// Outbound chunk requests buffered here so the game loop can send them.
    pub chunk_requests: Vec<(u32, u32)>,

    pub update_status: UpdateStatus,

    /// Low-resolution overview of the full map, sent by the server on connect.
    pub overview: Option<OverviewMap>,

    /// Whether the zoomed-out map overview is currently shown.
    pub show_map_overview: bool,
}

impl App {
    pub fn new(name: String) -> Self {
        Self {
            my_id: None,
            my_name: name,
            cursor: Position { x: 250.0, y: 250.0 },
            last_dir: Direction::Right,
            input: String::new(),
            bubbles: Vec::new(),
            remote_boats: HashMap::new(),
            world_info: None,
            loaded_chunks: HashMap::new(),
            pending_chunks: HashSet::new(),
            chunk_requests: Vec::new(),
            update_status: UpdateStatus::Unknown,
            overview: None,
            show_map_overview: false,
        }
    }

    // ── Page / viewport helpers ───────────────────────────────────────────────

    /// Which page the cursor is on (page coordinates).
    pub fn current_page(&self) -> (u32, u32) {
        (
            (self.cursor.x as u32) / PAGE_TILES_W,
            (self.cursor.y as u32) / PAGE_TILES_H,
        )
    }

    /// Tile origin (top-left corner) of the current page in world-tile coordinates.
    pub fn page_origin(&self) -> (u32, u32) {
        let (px, py) = self.current_page();
        (px * PAGE_TILES_W, py * PAGE_TILES_H)
    }

    // ── Chunk management ──────────────────────────────────────────────────────

    /// Queue chunk requests for the current page plus a `PREFETCH_BORDER`-wide border.
    ///
    /// Should be called after any move or after `WorldInfoEvent` is received.
    pub fn ensure_chunks_loaded(&mut self) {
        let (tile_w, tile_h, cs) = match &self.world_info {
            Some(wi) => (wi.tile_width, wi.tile_height, wi.chunk_size),
            None => return,
        };
        let chunks_x = tile_w.div_ceil(cs);
        let chunks_y = tile_h.div_ceil(cs);

        let (origin_x, origin_y) = self.page_origin();
        let page_cx_min = origin_x / cs;
        let page_cx_max = (origin_x + PAGE_TILES_W - 1) / cs;
        let page_cy_min = origin_y / cs;
        let page_cy_max = (origin_y + PAGE_TILES_H - 1) / cs;

        let cx_min = (page_cx_min as i32 - PREFETCH_BORDER).max(0) as u32;
        let cx_max =
            ((page_cx_max as i32 + PREFETCH_BORDER) as u32).min(chunks_x.saturating_sub(1));
        let cy_min = (page_cy_min as i32 - PREFETCH_BORDER).max(0) as u32;
        let cy_max =
            ((page_cy_max as i32 + PREFETCH_BORDER) as u32).min(chunks_y.saturating_sub(1));

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

    /// Try to move the cursor by `(dx, dy)`.  Returns `true` if the move was accepted.
    ///
    /// Moves onto `Tile::Coast` or `Tile::Land` are rejected when local chunk data is
    /// available.  Map-boundary clamping is always applied.
    pub fn move_cursor(&mut self, dx: f32, dy: f32) -> bool {
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

        // Local passability check (only when chunk data is available).
        if let Some(wi) = &self.world_info {
            let cs = wi.chunk_size;
            let cx = (new_x as u32) / cs;
            let cy = (new_y as u32) / cs;
            if let Some(chunk) = self.loaded_chunks.get(&(cx, cy)) {
                let local_col = (new_x as u32) % cs;
                let local_row = (new_y as u32) % cs;
                let idx = (local_row * cs + local_col) as usize;
                let tile = chunk.get(idx).copied().unwrap_or(Tile::Water);
                if tile != Tile::Water {
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
        self.cursor = Position { x: new_x, y: new_y };
        self.last_dir = new_dir;

        if self.current_page() != old_page {
            self.ensure_chunks_loaded();
        }
        true
    }

    // ── Chat bubbles ──────────────────────────────────────────────────────────

    pub fn push_bubble(&mut self, position: Position, text: String) {
        self.bubbles.push(Bubble {
            position,
            text,
            received_at: Instant::now(),
        });
    }

    pub fn expire_bubbles(&mut self) {
        self.bubbles.retain(|b| b.received_at.elapsed() < BUBBLE_TTL);
    }

    // ── Tile lookup ───────────────────────────────────────────────────────────

    /// Return the tile value at world-tile coordinates `(wx, wy)`, or `None`
    /// if the chunk is not yet loaded.
    pub fn tile_at_world(&self, wx: u32, wy: u32) -> Option<Tile> {
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

// ── Background update check ───────────────────────────────────────────────────

async fn fetch_latest_version() -> Option<String> {
    let client = reqwest::Client::builder()
        .user_agent("black-sea-client")
        .build()
        .ok()?;
    let resp: serde_json::Value = client
        .get("https://api.github.com/repos/dtennander/black-sea/releases/latest")
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    resp["tag_name"]
        .as_str()
        .map(|s: &str| s.trim_start_matches('v').to_string())
}

// ── Game loop ─────────────────────────────────────────────────────────────────

type ClientWs = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

pub async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    ws: &mut ClientWs,
    name: String,
) -> Result<()> {
    let mut app = App::new(name);
    // We already verified compatibility before entering the game, so mark it
    // as compatible immediately to avoid the "server version unknown" flicker.
    app.update_status = UpdateStatus::Compatible { patch_available: None };
    let (tx, mut rx) = mpsc::channel::<AppMsg>(64);

    // Spawn a background task to check for the latest release on GitHub.
    let tx_update = tx.clone();
    tokio::spawn(async move {
        if let Some(v) = fetch_latest_version().await {
            let _ = tx_update.send(AppMsg::LatestRelease(v)).await;
        }
    });

    // Spawn a task to forward keyboard events and periodic ticks.
    let tx_key = tx.clone();
    tokio::spawn(async move {
        use crossterm::event::{self, Event};
        use std::time::Duration;
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

        // Drain buffered chunk requests.
        for (cx, cy) in app.chunk_requests.drain(..).collect::<Vec<_>>() {
            send_event(ws, &GameEvent::MapChunkRequest { chunk_x: cx, chunk_y: cy }).await?;
        }

        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Some(AppMsg::Key(key)) => {
                        if crate::input::handle_key(&mut app, key, ws).await? {
                            break;
                        }
                    }
                    Some(AppMsg::Tick) => {}
                    Some(AppMsg::LatestRelease(v)) => {
                        let own = Version::parse(env!("GIT_VERSION"))
                            .unwrap_or(Version::new(0, 0, 0));
                        if let Ok(latest) = Version::parse(&v) {
                            if latest > own {
                                if matches!(
                                    app.update_status,
                                    UpdateStatus::Compatible { .. } | UpdateStatus::Unknown
                                ) {
                                    app.update_status =
                                        UpdateStatus::Compatible { patch_available: Some(v) };
                                }
                            }
                        }
                    }
                    None => break,
                }
            }

            result = recv_event(ws) => {
                match result? {
                    Some(event) => handle_server_event(&mut app, event),
                    None => break,
                }
            }
        }
    }

    Ok(())
}

// ── Server-event dispatcher ───────────────────────────────────────────────────

fn handle_server_event(app: &mut App, event: GameEvent) {
    match event {
        GameEvent::HelloEvent { your_id, start_position } => {
            app.my_id = Some(your_id);
            app.cursor = start_position;
        }

        GameEvent::WorldInfoEvent { tile_width, tile_height, chunk_size, .. } => {
            app.world_info = Some(WorldInfo { tile_width, tile_height, chunk_size });
            app.ensure_chunks_loaded();
        }

        GameEvent::MapChunkResponse { chunk_x, chunk_y, data } => {
            app.pending_chunks.remove(&(chunk_x, chunk_y));
            app.loaded_chunks.insert((chunk_x, chunk_y), data);
        }

        GameEvent::WorldStateEvent { boats } => {
            for (id, position, name) in boats {
                app.remote_boats.insert(
                    id,
                    RemoteBoat { position, name, last_dir: Direction::Right },
                );
            }
        }

        GameEvent::NameEvent { id, name } => {
            app.remote_boats
                .entry(id)
                .and_modify(|b| b.name = name.clone())
                .or_insert(RemoteBoat {
                    position: Position { x: 0.0, y: 0.0 },
                    name,
                    last_dir: Direction::Right,
                });
        }

        GameEvent::MoveEvent { id, position } => {
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
                app.remote_boats.insert(
                    id,
                    RemoteBoat {
                        position,
                        name: id.to_string(),
                        last_dir: Direction::Right,
                    },
                );
            }
        }

        GameEvent::ByeEvent { id } => {
            app.remote_boats.remove(&id);
        }

        GameEvent::SayEvent { position, text } => {
            let pos = position.unwrap_or_else(|| app.cursor.clone());
            app.push_bubble(pos, text);
        }

        GameEvent::ServerVersionEvent { version } => {
            let own = Version::parse(env!("GIT_VERSION"))
                .unwrap_or(Version::new(0, 0, 0));
            app.update_status = match Version::parse(&version) {
                Ok(srv) if srv.major == own.major && srv.minor == own.minor => {
                    UpdateStatus::Compatible { patch_available: None }
                }
                _ => UpdateStatus::Incompatible { server_version: version },
            };
        }

        GameEvent::OverviewMapEvent { width, height, data } => {
            app.overview = Some(OverviewMap { width, height, data });
        }

        // Handled in a follow-up session — ignore for now.
        GameEvent::AnchorPointsEvent { .. } => {}

        // Client should never receive these — ignore.
        GameEvent::RegisterEvent { .. } | GameEvent::MapChunkRequest { .. } => {}
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use black_sea_protocol::Tile;

    /// Build an App whose world_info matches a tiny 20×10 map with chunk_size=10.
    fn make_app_with_map(grid: Vec<Vec<Tile>>) -> App {
        let chunk_size = 10u32;
        let width = grid[0].len() as u32;
        let height = grid.len() as u32;

        let mut app = App::new("test".to_string());
        app.world_info = Some(WorldInfo {
            tile_width: width,
            tile_height: height,
            chunk_size,
        });

        // Load all chunks immediately.
        let chunks_x = width.div_ceil(chunk_size);
        let chunks_y = height.div_ceil(chunk_size);
        for cy in 0..chunks_y {
            for cx in 0..chunks_x {
                // Manually slice the chunk from the grid.
                let cs = chunk_size as usize;
                let origin_col = (cx * chunk_size) as usize;
                let origin_row = (cy * chunk_size) as usize;
                let mut data = Vec::with_capacity(cs * cs);
                for row in origin_row..origin_row + cs {
                    for col in origin_col..origin_col + cs {
                        let tile = grid
                            .get(row)
                            .and_then(|r| r.get(col))
                            .copied()
                            .unwrap_or(Tile::Water);
                        data.push(tile);
                    }
                }
                app.loaded_chunks.insert((cx, cy), data);
            }
        }
        app
    }

    fn all_water_app() -> App {
        let grid = vec![vec![Tile::Water; 20]; 10];
        make_app_with_map(grid)
    }

    fn app_with_land_at(land_col: usize, land_row: usize) -> App {
        let mut grid = vec![vec![Tile::Water; 20]; 10];
        grid[land_row][land_col] = Tile::Land;
        make_app_with_map(grid)
    }

    fn app_with_coast_at(coast_col: usize, coast_row: usize) -> App {
        let mut grid = vec![vec![Tile::Water; 20]; 10];
        grid[coast_row][coast_col] = Tile::Coast;
        make_app_with_map(grid)
    }

    // ── move_cursor ───────────────────────────────────────────────────────────

    #[test]
    fn move_cursor_water_succeeds() {
        let mut app = all_water_app();
        app.cursor = Position { x: 5.0, y: 5.0 };
        assert!(app.move_cursor(1.0, 0.0));
        assert_eq!(app.cursor.x as u32, 6);
    }

    #[test]
    fn move_cursor_into_land_blocked() {
        let mut app = app_with_land_at(6, 5);
        app.cursor = Position { x: 5.0, y: 5.0 };
        let moved = app.move_cursor(1.0, 0.0);
        assert!(!moved, "move into land should be blocked");
        assert_eq!(app.cursor.x as u32, 5, "cursor should not have moved");
    }

    #[test]
    fn move_cursor_into_coast_blocked() {
        let mut app = app_with_coast_at(6, 5);
        app.cursor = Position { x: 5.0, y: 5.0 };
        let moved = app.move_cursor(1.0, 0.0);
        assert!(!moved, "move into coast should be blocked");
    }

    #[test]
    fn move_cursor_clamps_to_map_bounds() {
        let mut app = all_water_app();
        app.cursor = Position { x: 0.0, y: 0.0 };
        // Try to move off the left/top edge.
        assert!(app.move_cursor(-10.0, -10.0)); // clamped to (0,0) → still "moved" (direction updated)
        assert_eq!(app.cursor.x as u32, 0);
        assert_eq!(app.cursor.y as u32, 0);
    }

    #[test]
    fn move_cursor_updates_direction() {
        let mut app = all_water_app();
        app.cursor = Position { x: 5.0, y: 5.0 };
        app.move_cursor(1.0, 0.0);
        assert_eq!(app.last_dir, Direction::Right);
        app.move_cursor(-1.0, 0.0);
        assert_eq!(app.last_dir, Direction::Left);
        app.move_cursor(0.0, -1.0);
        assert_eq!(app.last_dir, Direction::Up);
        app.move_cursor(0.0, 1.0);
        assert_eq!(app.last_dir, Direction::Down);
    }

    // ── expire_bubbles ────────────────────────────────────────────────────────

    #[test]
    fn expire_bubbles_removes_stale() {
        let mut app = all_water_app();
        // Add a bubble that is already stale (received 10 seconds ago).
        app.bubbles.push(Bubble {
            position: Position { x: 0.0, y: 0.0 },
            text: "old".into(),
            received_at: Instant::now()
                .checked_sub(Duration::from_secs(10))
                .unwrap_or(Instant::now()),
        });
        // Add a fresh bubble.
        app.bubbles.push(Bubble {
            position: Position { x: 0.0, y: 0.0 },
            text: "fresh".into(),
            received_at: Instant::now(),
        });
        app.expire_bubbles();
        assert_eq!(app.bubbles.len(), 1);
        assert_eq!(app.bubbles[0].text, "fresh");
    }

}
