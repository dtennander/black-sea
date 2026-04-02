mod progress;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use black_sea::{GameEvent, Position, recv_event, send_event};
use rand::Rng;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};

// ── Map constants ─────────────────────────────────────────────────────────────

/// Bounding box for the Stockholm inner/mid archipelago (WGS-84).
#[cfg(feature = "server-map")]
const BBOX_MIN_LAT: f64 = 58.80;
#[cfg(feature = "server-map")]
const BBOX_MAX_LAT: f64 = 59.80;
#[cfg(feature = "server-map")]
const BBOX_MIN_LON: f64 = 17.50;
#[cfg(feature = "server-map")]
const BBOX_MAX_LON: f64 = 20.00;

/// Real-world width of the bounding box in degrees lon (~170 km).
/// Each tile covers this many metres at 8500 tiles wide: ~20 m/tile.
const MAP_TILES_W: u32 = 8500;
const MAP_TILES_H: u32 = 5500;

/// Chunk size the server advertises (square).
const CHUNK_SIZE: u32 = 50;

/// Approximate metres per tile (used for the client info event).
const METRES_PER_TILE: f32 = 20.0;

/// Tile values stored in the map grid.
pub const TILE_WATER: u8 = 0;
pub const TILE_COAST: u8 = 1;
pub const TILE_LAND: u8 = 2;

// ── Game constants ────────────────────────────────────────────────────────────

const MIN_SEPARATION: f32 = 5.0;
const MAX_PLACEMENT_ATTEMPTS: usize = 1000;
const SPAWN_ANCHOR_X: f32 = 4590.0;
const SPAWN_ANCHOR_Y: f32 = 2728.0;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

// ── Map grid ──────────────────────────────────────────────────────────────────

/// The rasterized world map.  Row 0 is the northernmost row.
/// Indexed as `grid[row][col]`, i.e. `grid[y][x]`.
pub struct MapGrid {
    pub grid: Vec<Vec<u8>>, // [row][col]
    pub width: u32,
    pub height: u32,
    pub chunk_size: u32,
}

impl MapGrid {
    /// Return the tile at grid coordinates (col, row).  Out-of-bounds → WATER.
    pub fn tile_at(&self, col: u32, row: u32) -> u8 {
        self.grid
            .get(row as usize)
            .and_then(|r| r.get(col as usize))
            .copied()
            .unwrap_or(TILE_WATER)
    }

    /// Convert a floating-point game Position (each axis 0..tile_count) to a tile.
    pub fn tile_at_pos(&self, pos: &Position) -> u8 {
        let col = pos.x.clamp(0.0, self.width as f32 - 1.0) as u32;
        let row = pos.y.clamp(0.0, self.height as f32 - 1.0) as u32;
        self.tile_at(col, row)
    }

    pub fn is_passable(&self, pos: &Position) -> bool {
        self.tile_at_pos(pos) != TILE_LAND && self.tile_at_pos(pos) != TILE_COAST
    }

    /// Extract a chunk as a flat Vec<u8> (row-major, chunk_size² bytes).
    pub fn chunk_data(&self, chunk_x: u32, chunk_y: u32) -> Vec<u8> {
        let cs = self.chunk_size as usize;
        let origin_col = (chunk_x * self.chunk_size) as usize;
        let origin_row = (chunk_y * self.chunk_size) as usize;
        let mut data = Vec::with_capacity(cs * cs);
        for row in origin_row..origin_row + cs {
            for col in origin_col..origin_col + cs {
                let tile = self
                    .grid
                    .get(row)
                    .and_then(|r| r.get(col))
                    .copied()
                    .unwrap_or(TILE_WATER);
                data.push(tile);
            }
        }
        data
    }
}

// ── Map loading ───────────────────────────────────────────────────────────────

#[cfg(feature = "server-map")]
mod map_loader {
    use super::*;
    use anyhow::Context;
    use geo::BoundingRect;
    use geo::Simplify;
    use geo::geometry::{Coord, LineString};
    use rayon::prelude::*;
    use shapefile::Shape;
    use std::io::{Cursor, Read};

    use crate::progress::{make_count_bar, make_download_bar};

    /// Half a tile in degrees — detail finer than this is invisible in the output.
    /// At 500 tiles over ~111 km (lat) each tile ≈ 222 m; half ≈ 111 m ≈ 0.001°.
    const SIMPLIFY_EPSILON: f64 = 0.001;

    /// Download and rasterize the OSM land-polygon Shapefile into a MapGrid.
    pub fn load_map() -> Result<MapGrid> {
        println!("[map] Downloading OSM land polygons...");
        let zip_bytes = download_land_polygons()?;

        println!("[map] Parsing Shapefile from zip...");
        let polygons = parse_shapefile_from_zip(&zip_bytes)?;

        // Simplify polygons to remove sub-tile detail — drastically reduces
        // vertex counts on complex coastlines (e.g. mainland Sweden) before
        // the scanline rasterizer has to process them.
        println!(
            "[map] Simplifying {} polygons (epsilon={SIMPLIFY_EPSILON})...",
            polygons.len()
        );
        let bar = make_count_bar(polygons.len() as u64, "polygons simplified", 2000);
        let polygons: Vec<geo::geometry::Polygon<f64>> = polygons
            .into_par_iter()
            .map(|p| {
                let s = p.simplify(SIMPLIFY_EPSILON);
                bar.inc();
                s
            })
            .collect();
        bar.finish();
        let total_verts: usize = polygons.iter().map(|p| p.exterior().0.len()).sum();
        println!(
            "[map] After simplification: {} total exterior vertices",
            total_verts
        );

        println!(
            "[map] Rasterizing {} polygons to {}x{} grid...",
            polygons.len(),
            MAP_TILES_W,
            MAP_TILES_H
        );
        let grid = rasterize(&polygons);

        Ok(MapGrid {
            grid,
            width: MAP_TILES_W,
            height: MAP_TILES_H,
            chunk_size: CHUNK_SIZE,
        })
    }

    // ── Download / cache ──────────────────────────────────────────────────────

    const OSM_URL: &str =
        "https://osmdata.openstreetmap.de/download/land-polygons-complete-4326.zip";
    const ZIP_NAME: &str = "land-polygons-complete-4326.zip";
    const ETAG_NAME: &str = "land-polygons-complete-4326.zip.etag";

    /// Resolve the cache directory from `BLACK_SEA_CACHE_DIR` (default: `./osm-cache`).
    fn cache_dir() -> std::path::PathBuf {
        let dir =
            std::env::var("BLACK_SEA_CACHE_DIR").unwrap_or_else(|_| "./osm-cache".to_string());
        let path = std::path::PathBuf::from(dir);
        if let Err(e) = std::fs::create_dir_all(&path) {
            eprintln!(
                "[map] Warning: could not create cache dir {}: {e}",
                path.display()
            );
        }
        path
    }

    /// Send a HEAD request and return the ETag header value, if present.
    fn fetch_remote_etag() -> Result<Option<String>> {
        let resp = ureq::head(OSM_URL)
            .call()
            .context("failed to HEAD OSM URL")?;
        Ok(resp
            .headers()
            .get("etag")
            .map(|v| v.to_str().unwrap_or("").to_string()))
    }

    /// Try to load the cached zip from disk.  Returns `Some(bytes)` only if
    /// the zip file exists *and* its stored ETag matches `remote_etag`.
    fn load_from_cache(
        cache_dir: &std::path::Path,
        remote_etag: &Option<String>,
    ) -> Option<Vec<u8>> {
        let zip_path = cache_dir.join(ZIP_NAME);
        let etag_path = cache_dir.join(ETAG_NAME);

        if !zip_path.exists() {
            println!("[map] No cached file found");
            return None;
        }

        // If we have a remote ETag, validate it against the stored one.
        if let Some(remote) = remote_etag {
            match std::fs::read_to_string(&etag_path) {
                Ok(stored) if stored.trim() == remote.trim() => {
                    println!("[map] Cache valid (ETag matches), loading from disk...");
                }
                Ok(stored) => {
                    println!(
                        "[map] Cache stale (ETag mismatch: stored={}, remote={})",
                        stored.trim(),
                        remote.trim()
                    );
                    return None;
                }
                Err(_) => {
                    println!("[map] Cache ETag file missing or unreadable, re-downloading");
                    return None;
                }
            }
        } else {
            // No remote ETag available — trust the cached file if it exists.
            println!("[map] Remote ETag unavailable, using cached file as-is");
        }

        match std::fs::read(&zip_path) {
            Ok(bytes) => {
                println!("[map] Loaded {} MiB from cache", bytes.len() / 1_048_576);
                Some(bytes)
            }
            Err(e) => {
                eprintln!("[map] Warning: failed to read cache file: {e}");
                None
            }
        }
    }

    /// Write the zip bytes and ETag sidecar to the cache directory.
    /// Failures are logged as warnings but are non-fatal.
    fn save_to_cache(cache_dir: &std::path::Path, bytes: &[u8], etag: &Option<String>) {
        let zip_path = cache_dir.join(ZIP_NAME);
        if let Err(e) = std::fs::write(&zip_path, bytes) {
            eprintln!(
                "[map] Warning: could not write cache file {}: {e}",
                zip_path.display()
            );
            return;
        }
        println!("[map] Saved {} MiB to cache", bytes.len() / 1_048_576);

        if let Some(tag) = etag {
            let etag_path = cache_dir.join(ETAG_NAME);
            if let Err(e) = std::fs::write(&etag_path, tag) {
                eprintln!(
                    "[map] Warning: could not write ETag file {}: {e}",
                    etag_path.display()
                );
            }
        }
    }

    /// Fetch the zip from the network, streaming it into memory with a progress bar.
    fn fetch_from_network() -> Result<Vec<u8>> {
        const TWO_GIB: u64 = 2 * 1024 * 1024 * 1024;
        const CHUNK: usize = 65_536; // 64 KiB read chunks
        const LOG_INTERVAL_BYTES: u64 = 100 * 1024 * 1024; // log every 100 MiB (non-TTY)

        let mut body = ureq::get(OSM_URL)
            .call()
            .context("failed to download OSM land polygon zip")?
            .into_body();

        let total = body.content_length();
        let mut reader = body.with_config().limit(TWO_GIB).reader();
        let mut bytes: Vec<u8> = match total {
            Some(n) => Vec::with_capacity(n as usize),
            None => Vec::new(),
        };

        let mut bar = make_download_bar(total, LOG_INTERVAL_BYTES);
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = reader.read(&mut buf).context("error reading download")?;
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&buf[..n]);
            bar.set_position(bytes.len() as u64);
        }
        bar.finish();

        println!("[map] Downloaded {} MiB", bytes.len() / 1_048_576);
        Ok(bytes)
    }

    /// Top-level entry point: return the zip bytes, using the local cache when valid.
    fn download_land_polygons() -> Result<Vec<u8>> {
        let cache = cache_dir();

        println!("[map] Checking remote ETag...");
        let remote_etag = fetch_remote_etag().unwrap_or_else(|e| {
            eprintln!("[map] Warning: could not fetch remote ETag ({e}), will re-download");
            None
        });

        // Try to use the cached file.
        if let Some(bytes) = load_from_cache(&cache, &remote_etag) {
            return Ok(bytes);
        }

        // Cache miss — download from network then persist.
        println!("[map] Downloading OSM land polygons...");
        let bytes = fetch_from_network()?;
        save_to_cache(&cache, &bytes, &remote_etag);
        Ok(bytes)
    }

    // ── Shapefile parsing ─────────────────────────────────────────────────────

    fn parse_shapefile_from_zip(zip_bytes: &[u8]) -> Result<Vec<geo::geometry::Polygon<f64>>> {
        let cursor = Cursor::new(zip_bytes);
        let mut archive = zip::ZipArchive::new(cursor).context("failed to open zip archive")?;

        let mut shp_buf: Option<Vec<u8>> = None;
        let mut dbf_buf: Option<Vec<u8>> = None;
        let mut shx_buf: Option<Vec<u8>> = None;

        for i in 0..archive.len() {
            let mut f = archive.by_index(i)?;
            let name = f.name().to_lowercase();
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            if name.ends_with(".shp") {
                shp_buf = Some(buf);
            } else if name.ends_with(".dbf") {
                dbf_buf = Some(buf);
            } else if name.ends_with(".shx") {
                shx_buf = Some(buf);
            }
        }

        let shp = shp_buf.context("no .shp file found in zip")?;
        let dbf = dbf_buf.context("no .dbf file found in zip")?;
        let shx = shx_buf.context("no .shx file found in zip")?;

        let shape_reader = shapefile::ShapeReader::with_shx(Cursor::new(shp), Cursor::new(shx))
            .context("failed to create ShapeReader")?;
        let dbase_reader = shapefile::dbase::Reader::new(Cursor::new(dbf))
            .context("failed to create dbase Reader")?;
        let mut reader = shapefile::Reader::new(shape_reader, dbase_reader);

        let mut polys: Vec<geo::geometry::Polygon<f64>> = Vec::new();

        for result in reader.iter_shapes_and_records() {
            let (shape, _) = result?;
            if let Shape::Polygon(poly) = shape {
                let bbox = poly.bbox();
                if bbox.max.x < BBOX_MIN_LON
                    || bbox.min.x > BBOX_MAX_LON
                    || bbox.max.y < BBOX_MIN_LAT
                    || bbox.min.y > BBOX_MAX_LAT
                {
                    continue;
                }
                for ring in poly.rings() {
                    let coords: Vec<Coord<f64>> = ring
                        .points()
                        .iter()
                        .map(|p| Coord { x: p.x, y: p.y })
                        .collect();
                    if coords.len() < 3 {
                        continue;
                    }
                    // TODO: handle interior rings (holes) so lakes / fjords
                    // inside islands are correctly left as water.
                    polys.push(geo::geometry::Polygon::new(LineString::new(coords), vec![]));
                }
            }
        }

        println!("[map] {} polygons after bbox clip", polys.len());
        Ok(polys)
    }

    // ── Rasterization ─────────────────────────────────────────────────────────

    fn rasterize(polygons: &[geo::geometry::Polygon<f64>]) -> Vec<Vec<u8>> {
        let w = MAP_TILES_W as usize;
        let h = MAP_TILES_H as usize;

        // ── First pass: scanline fill ─────────────────────────────────────────
        //
        // Instead of asking per-cell "which polygon contains me?" (O(cells × polys)),
        // we flip the loop: for each polygon, compute which cells it covers via a
        // scanline intersection algorithm.  Complexity: O(total_vertices + filled_cells),
        // which is linear in the data rather than quadratic.
        //
        // For each scanline row we accumulate (col_start, col_end) fill intervals.
        // These are later applied to the grid in a parallel pass.
        //
        // Because we flatten each shapefile ring into a separate polygon
        // (see TODO in parser), the even-odd fill rule is not needed here —
        // every polygon is treated as a solid filled shape.

        // Build fill intervals per row: Vec<Vec<(usize, usize)>>
        // Indexed as fill_intervals[row] = [(col_start, col_end), ...]
        let mut fill_intervals: Vec<Vec<(usize, usize)>> = vec![Vec::new(); h];

        // Collect all fill intervals from all polygons.
        // This part must be single-threaded because we mutate fill_intervals by row.
        // (Parallelising over polygons would need per-row mutexes; the apply step
        //  below is where rayon parallelism is used instead.)
        let bar = make_count_bar(polygons.len() as u64, "polygons", 500);

        for poly in polygons.iter() {
            scanline_fill_polygon(poly, w, h, &mut fill_intervals);
            bar.inc();
        }

        bar.finish();

        // ── Apply fill intervals in parallel ──────────────────────────────────
        // Each row is independent so this is trivially data-parallel.
        let mut grid: Vec<Vec<u8>> = fill_intervals
            .into_par_iter()
            .map(|intervals| {
                let mut row = vec![TILE_WATER; w];
                for (c0, c1) in intervals {
                    for cell in &mut row[c0..=c1.min(w - 1)] {
                        *cell = TILE_LAND;
                    }
                }
                row
            })
            .collect();

        // ── Second pass: coastline = water cell adjacent to land (4-connected) ─
        let land_copy = grid.clone();
        for row in 0..h {
            for col in 0..w {
                if land_copy[row][col] == TILE_WATER {
                    let has_land_neighbour = [
                        row.checked_sub(1).map(|r| (r, col)),
                        if row + 1 < h {
                            Some((row + 1, col))
                        } else {
                            None
                        },
                        col.checked_sub(1).map(|c| (row, c)),
                        if col + 1 < w {
                            Some((row, col + 1))
                        } else {
                            None
                        },
                    ]
                    .into_iter()
                    .flatten()
                    .any(|(r, c)| land_copy[r][c] == TILE_LAND);
                    if has_land_neighbour {
                        grid[row][col] = TILE_COAST;
                    }
                }
            }
        }

        grid
    }

    /// Compute scanline fill intervals for a single polygon and push them into
    /// `fill_intervals[row]`.
    ///
    /// Algorithm:
    ///   For each grid row, compute the latitude of that row's centre.
    ///   Walk every edge of the polygon's exterior ring.  If the edge crosses
    ///   this latitude, compute the x (longitude) of the intersection.
    ///   Collect all intersections for the row, sort them, then fill between
    ///   each consecutive pair (even-odd / non-zero both work for simple rings).
    fn scanline_fill_polygon(
        poly: &geo::geometry::Polygon<f64>,
        w: usize,
        h: usize,
        fill_intervals: &mut Vec<Vec<(usize, usize)>>,
    ) {
        // Determine the row range this polygon's bbox overlaps — skip rows outside.
        let bbox = match poly.bounding_rect() {
            Some(b) => b,
            None => return,
        };

        // Convert bbox lat extents to row indices (clamped to grid).
        // row 0 = northernmost (BBOX_MAX_LAT), row h-1 = southernmost (BBOX_MIN_LAT).
        let row_of_lat = |lat: f64| -> usize {
            let frac = (BBOX_MAX_LAT - lat) / (BBOX_MAX_LAT - BBOX_MIN_LAT);
            (frac * (h - 1) as f64).round() as usize
        };
        let col_of_lon = |lon: f64| -> usize {
            let frac = (lon - BBOX_MIN_LON) / (BBOX_MAX_LON - BBOX_MIN_LON);
            (frac * (w - 1) as f64).round() as usize
        };

        // Clamp to grid bounds.
        let row_min = row_of_lat(bbox.max().y).saturating_sub(1).min(h - 1);
        let row_max = (row_of_lat(bbox.min().y) + 1).min(h - 1);

        let coords: &[Coord<f64>] = poly.exterior().0.as_slice();
        let n = coords.len();
        if n < 2 {
            return;
        }

        for row in row_min..=row_max {
            let lat = BBOX_MAX_LAT - (row as f64 / (h - 1) as f64) * (BBOX_MAX_LAT - BBOX_MIN_LAT);

            // Collect x (lon) intersections of all edges with this scanline.
            let mut xs: Vec<f64> = Vec::new();
            for i in 0..n {
                let a = coords[i];
                let b = coords[(i + 1) % n];
                let (y0, y1) = (a.y, b.y);
                // Edge must straddle the scanline (strictly on one side each).
                if (y0 <= lat && y1 > lat) || (y1 <= lat && y0 > lat) {
                    // Linear interpolation of x at lat.
                    let t = (lat - y0) / (y1 - y0);
                    xs.push(a.x + t * (b.x - a.x));
                }
            }

            if xs.len() < 2 {
                continue;
            }
            xs.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());

            // Fill between each pair of intersections.
            for pair in xs.chunks_exact(2) {
                let x0 = pair[0];
                let x1 = pair[1];
                // Skip entirely outside bbox.
                if x1 < BBOX_MIN_LON || x0 > BBOX_MAX_LON {
                    continue;
                }
                let c0 = col_of_lon(x0.max(BBOX_MIN_LON));
                let c1 = col_of_lon(x1.min(BBOX_MAX_LON));
                if c0 <= c1 {
                    fill_intervals[row].push((c0, c1));
                }
            }
        }
    }
}

/// Fallback when compiled without the server-map feature: empty all-water grid.
#[cfg(not(feature = "server-map"))]
mod map_loader {
    use super::*;

    pub fn load_map() -> Result<MapGrid> {
        println!("[map] server-map feature not enabled — using empty all-water map");
        let grid = vec![vec![TILE_WATER; MAP_TILES_W as usize]; MAP_TILES_H as usize];
        Ok(MapGrid {
            grid,
            width: MAP_TILES_W,
            height: MAP_TILES_H,
            chunk_size: CHUNK_SIZE,
        })
    }
}

// ── Broadcast types ───────────────────────────────────────────────────────────

#[derive(Clone)]
struct Envelope {
    sender_id: u64,
    event: GameEvent,
}

#[derive(Clone)]
struct BoatEntry {
    position: Position,
    name: String,
}

type BoatMap = Arc<Mutex<HashMap<u64, BoatEntry>>>;
type SharedMap = Arc<MapGrid>;

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    // Load (or generate) the map before accepting any connections.
    let map_grid: SharedMap = Arc::new(map_loader::load_map()?);
    println!(
        "[map] Ready: {}×{} tiles, chunk_size={}",
        map_grid.width, map_grid.height, map_grid.chunk_size
    );

    let listener = TcpListener::bind("0.0.0.0:7456").await?;
    let (tx, _) = broadcast::channel::<Envelope>(64);
    let boats: BoatMap = Arc::new(Mutex::new(HashMap::new()));
    println!("Running Server");

    loop {
        let (socket, addr) = listener.accept().await?;
        println!("New connection from: {addr}");

        let tx = tx.clone();
        let rx = tx.subscribe();
        let boats = Arc::clone(&boats);
        let map = Arc::clone(&map_grid);
        tokio::spawn(async move {
            match accept_async(socket).await {
                Ok(ws) => {
                    if let Err(e) = handle(ws, tx, rx, boats, map).await {
                        eprintln!("Error handling connection from {addr}: {e}");
                    }
                }
                Err(e) => eprintln!("WebSocket handshake failed from {addr}: {e}"),
            }
        });
    }
}

// ── Spawn helper ──────────────────────────────────────────────────────────────

/// Pick a random water position near Sandhamn, expanding the search radius until a free spot is found.
fn find_free_position(boats: &HashMap<u64, BoatEntry>, map: &MapGrid) -> Option<Position> {
    let mut rng = rand::rng();
    let mut radius = 10.0_f32;
    let max_radius = (map.width.max(map.height) as f32) / 2.0;

    while radius <= max_radius {
        for _ in 0..MAX_PLACEMENT_ATTEMPTS {
            let candidate = Position {
                x: (SPAWN_ANCHOR_X + rng.random_range(-radius..radius))
                    .clamp(0.0, map.width as f32 - 1.0),
                y: (SPAWN_ANCHOR_Y + rng.random_range(-radius..radius))
                    .clamp(0.0, map.height as f32 - 1.0),
            };
            if !map.is_passable(&candidate) {
                continue;
            }
            let too_close = boats.values().any(|entry| {
                let dx = entry.position.x - candidate.x;
                let dy = entry.position.y - candidate.y;
                (dx * dx + dy * dy).sqrt() < MIN_SEPARATION
            });
            if !too_close {
                return Some(candidate);
            }
        }
        radius *= 2.0;
    }
    None
}

// ── Per-client handler ────────────────────────────────────────────────────────

async fn handle(
    mut ws: WebSocketStream<TcpStream>,
    tx: broadcast::Sender<Envelope>,
    mut rx: broadcast::Receiver<Envelope>,
    boats: BoatMap,
    map: SharedMap,
) -> Result<()> {
    let self_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

    // Wait for RegisterEvent.
    let name = loop {
        match recv_event(&mut ws).await? {
            Some(GameEvent::RegisterEvent { name }) => break name,
            Some(_) => {}
            None => return Ok(()),
        }
    };

    // Assign a free starting position.
    let start_position = {
        let pos = {
            let mut m = boats.lock().unwrap();
            match find_free_position(&m, &map) {
                Some(pos) => {
                    m.insert(
                        self_id,
                        BoatEntry {
                            position: pos.clone(),
                            name: name.clone(),
                        },
                    );
                    Some(pos)
                }
                None => None,
            }
        };
        match pos {
            Some(pos) => pos,
            None => {
                let _ = ws
                    .close(Some(CloseFrame {
                        code: CloseCode::Again,
                        reason: "World is full".into(),
                    }))
                    .await;
                return Ok(());
            }
        }
    };

    // HelloEvent
    send_event(
        &mut ws,
        &GameEvent::HelloEvent {
            your_id: self_id,
            start_position,
        },
    )
    .await?;

    // WorldInfoEvent — tells the client about the map dimensions and chunk layout.
    send_event(
        &mut ws,
        &GameEvent::WorldInfoEvent {
            tile_width: map.width,
            tile_height: map.height,
            chunk_size: map.chunk_size,
            meters_per_tile: METRES_PER_TILE,
        },
    )
    .await?;

    // WorldStateEvent — snapshot of all other boats.
    let snapshot: Vec<(u64, Position, String)> = boats
        .lock()
        .unwrap()
        .iter()
        .filter(|(id, _)| **id != self_id)
        .map(|(id, entry)| (*id, entry.position.clone(), entry.name.clone()))
        .collect();
    send_event(&mut ws, &GameEvent::WorldStateEvent { boats: snapshot }).await?;

    // Broadcast NameEvent so existing clients learn about the new player.
    let _ = tx.send(Envelope {
        sender_id: self_id,
        event: GameEvent::NameEvent {
            id: self_id,
            name: name.clone(),
        },
    });

    println!("Client {self_id} registered as '{name}'");

    let loop_result: Result<()> = async {
        loop {
            tokio::select! {
                result = recv_event(&mut ws) => {
                    match result? {
                        Some(event) => {
                            let broadcast_event = match event {
                                GameEvent::SayEvent { position: client_pos, ref text } => {
                                    let authoritative_pos = boats
                                        .lock()
                                        .unwrap()
                                        .get(&self_id)
                                        .map(|e| e.position.clone())
                                        .or(client_pos);
                                    match &authoritative_pos {
                                        Some(p) => println!("[SAY] ({}, {}): {}", p.x, p.y, text),
                                        None    => println!("[SAY] (unknown): {}", text),
                                    }
                                    GameEvent::SayEvent { position: authoritative_pos, text: text.clone() }
                                }

                                GameEvent::MoveEvent { position, .. } => {
                                    // Silently drop moves onto land or coastline.
                                    if !map.is_passable(&position) {
                                        continue;
                                    }
                                    let mut m = boats.lock().unwrap();
                                    if let Some(entry) = m.get_mut(&self_id) {
                                        entry.position = position.clone();
                                    }
                                    GameEvent::MoveEvent { id: self_id, position }
                                }

                                // Serve a single map chunk on demand.
                                GameEvent::MapChunkRequest { chunk_x, chunk_y } => {
                                    let data = map.chunk_data(chunk_x, chunk_y);
                                    send_event(
                                        &mut ws,
                                        &GameEvent::MapChunkResponse { chunk_x, chunk_y, data },
                                    )
                                    .await?;
                                    continue; // response already sent directly, nothing to broadcast
                                }

                                // Clients should not send these — ignore.
                                GameEvent::RegisterEvent { .. }
                                | GameEvent::HelloEvent { .. }
                                | GameEvent::WorldStateEvent { .. }
                                | GameEvent::WorldInfoEvent { .. }
                                | GameEvent::NameEvent { .. }
                                | GameEvent::ByeEvent { .. }
                                | GameEvent::MapChunkResponse { .. } => continue,
                            };
                            let _ = tx.send(Envelope { sender_id: self_id, event: broadcast_event });
                        }
                        None => {
                            println!("Client {self_id} disconnected");
                            break;
                        }
                    }
                }

                result = rx.recv() => {
                    match result {
                        Ok(envelope) if envelope.sender_id != self_id => {
                            send_event(&mut ws, &envelope.event).await?;
                        }
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            eprintln!("Client {self_id} lagged, dropped {n} events");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
        Ok(())
    }.await;

    boats.lock().unwrap().remove(&self_id);
    let _ = tx.send(Envelope {
        sender_id: self_id,
        event: GameEvent::ByeEvent { id: self_id },
    });

    loop_result
}
