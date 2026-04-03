mod handler;
mod spawn;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use black_sea_protocol::MapGrid;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::accept_async;

use handler::{BoatMap, Envelope, handle};

/// Approximate metres per tile — passed to clients via `WorldInfoEvent`.
const METRES_PER_TILE: f32 = 20.0;

#[tokio::main]
async fn main() -> Result<()> {
    let map_grid: Arc<MapGrid> = Arc::new(load_map()?);
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
                    if let Err(e) = handle(ws, tx, rx, boats, map, METRES_PER_TILE).await {
                        eprintln!("Error handling connection from {addr}: {e}");
                    }
                }
                Err(e) => eprintln!("WebSocket handshake failed from {addr}: {e}"),
            }
        });
    }
}

// ── Map loading ───────────────────────────────────────────────────────────────

fn load_map() -> Result<MapGrid> {
    black_sea_map_loader::load_map()
}
