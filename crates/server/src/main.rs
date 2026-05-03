mod handler;
mod metrics;
mod spawn;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use black_sea_protocol::{AnchorPoint, MapGrid, Position, Tile};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::accept_async;

use handler::{BoatMap, Envelope, OverviewData, handle};

/// Approximate metres per tile — passed to clients via `WorldInfoEvent`.
const METRES_PER_TILE: f32 = 20.0;

#[derive(Deserialize)]
struct AnchorRow {
    name: String,
    x: f32,
    y: f32,
    note: Option<String>,
}

fn load_anchor_points(path: &str) -> Vec<AnchorPoint> {
    let mut reader = match csv::Reader::from_path(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[anchors] Could not read {path}: {e}");
            return Vec::new();
        }
    };
    let points: Vec<AnchorPoint> = reader
        .deserialize::<AnchorRow>()
        .enumerate()
        .filter_map(|(i, result)| {
            result
                .map_err(|e| eprintln!("[anchors] Skipping row {i}: {e}"))
                .ok()
                .map(|row| AnchorPoint {
                    id: i as u32,
                    name: row.name,
                    position: Position { x: row.x, y: row.y },
                    note: row.note.filter(|s| !s.is_empty()),
                })
        })
        .collect();
    println!(
        "[anchors] Loaded {} anchor points from {path}",
        points.len()
    );
    points
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("Starting server on version {}", env!("CLIENT_VERSION"));
    let (full_grid, overview_grid) = black_sea_map_loader::load_map()?;
    let map_grid: Arc<MapGrid> = Arc::new(full_grid);
    println!(
        "[map] Ready: {}×{} tiles, chunk_size={}",
        map_grid.width, map_grid.height, map_grid.chunk_size
    );

    metrics::init();
    tokio::spawn(async {
        if let Err(e) = metrics::serve_metrics("0.0.0.0:9090").await {
            eprintln!("[metrics] Server error: {e}");
        }
    });

    let csv_path = std::env::var("ANCHORINGS_CSV").unwrap_or_else(|_| "anchorings.csv".to_string());
    let anchor_points: Arc<Vec<AnchorPoint>> = Arc::new(load_anchor_points(&csv_path));

    tokio::spawn(async move {
        let admin_csv = PathBuf::from(&csv_path);
        let admin_stats = black_sea_admin_tool::Stats {
            active_connections: Arc::new(|| metrics::ACTIVE_CONNECTIONS.get()),
            total_connections: Arc::new(|| metrics::TOTAL_CONNECTIONS.get()),
        };
        if let Err(e) =
            black_sea_admin_tool::serve("0.0.0.0:8080", admin_csv, admin_stats, "/admin").await
        {
            eprintln!("[admin] Server error: {e}");
        }
    });

    let listener = TcpListener::bind("0.0.0.0:7456").await?;
    let (tx, _) = broadcast::channel::<Envelope>(64);
    let boats: BoatMap = Arc::new(Mutex::new(HashMap::new()));
    println!("Running Server");

    let overview: Arc<OverviewData> = Arc::new(OverviewData {
        width: overview_grid.width,
        height: overview_grid.height,
        data: overview_grid
            .grid
            .into_iter()
            .flatten()
            .collect::<Vec<Tile>>(),
    });

    loop {
        let (socket, addr) = listener.accept().await?;
        println!("New connection from: {addr}");
        metrics::TOTAL_CONNECTIONS.inc();
        metrics::ACTIVE_CONNECTIONS.inc();

        let tx = tx.clone();
        let rx = tx.subscribe();
        let boats = Arc::clone(&boats);
        let map = Arc::clone(&map_grid);
        let ov = Arc::clone(&overview);
        let anchors = Arc::clone(&anchor_points);
        tokio::spawn(async move {
            match accept_async(socket).await {
                Ok(ws) => {
                    if let Err(e) =
                        handle(ws, tx, rx, boats, map, ov, anchors, METRES_PER_TILE).await
                    {
                        eprintln!("Error handling connection from {addr}: {e}");
                    }
                }
                Err(e) => eprintln!("WebSocket handshake failed from {addr}: {e}"),
            }
        });
    }
}
