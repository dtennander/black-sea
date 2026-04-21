mod anchorings;
mod handler;
mod metrics;
mod spawn;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use black_sea_protocol::{AnchorPoint, MapGrid, Tile};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::accept_async;

use handler::{BoatMap, Envelope, OverviewData, handle};

/// Approximate metres per tile — passed to clients via `WorldInfoEvent`.
const METRES_PER_TILE: f32 = 20.0;

#[tokio::main]
async fn main() -> Result<()> {
    let (full_grid, overview_grid) = black_sea_map_loader::load_map()?;
    let map_grid: Arc<MapGrid> = Arc::new(full_grid);
    println!(
        "[map] Ready: {}×{} tiles, chunk_size={}",
        map_grid.width, map_grid.height, map_grid.chunk_size
    );

    let overview: Arc<OverviewData> = Arc::new(OverviewData {
        width: overview_grid.width,
        height: overview_grid.height,
        data: overview_grid
            .grid
            .into_iter()
            .flatten()
            .collect::<Vec<Tile>>(),
    });

    let anchorings_path = PathBuf::from(
        std::env::var("BLACK_SEA_ANCHORINGS").unwrap_or_else(|_| "anchorings.csv".to_string()),
    );
    let anchor_points: Arc<Vec<AnchorPoint>> = Arc::new(
        anchorings::load_anchorings(&anchorings_path).unwrap_or_else(|e| {
            eprintln!(
                "[anchorings] Failed to load {}: {e}",
                anchorings_path.display()
            );
            Vec::new()
        }),
    );

    let listener = TcpListener::bind("0.0.0.0:7456").await?;
    let (tx, _) = broadcast::channel::<Envelope>(64);
    let boats: BoatMap = Arc::new(Mutex::new(HashMap::new()));
    println!("Running Server");

    metrics::init();
    tokio::spawn(async {
        if let Err(e) = metrics::serve_metrics("0.0.0.0:9090").await {
            eprintln!("[metrics] Server error: {e}");
        }
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
