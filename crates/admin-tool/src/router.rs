use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use crate::handlers::{get_anchors, save_anchors, serve_dashboard, serve_editor};

pub struct Stats {
    pub active_connections: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub total_connections: Arc<dyn Fn() -> u64 + Send + Sync>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct AnchorPoint {
    pub(crate) name: String,
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) note: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AnchorLatLon {
    pub(crate) name: String,
    pub(crate) lat: f64,
    pub(crate) lon: f64,
    pub(crate) note: String,
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) csv_path: Arc<Mutex<PathBuf>>,
    pub(crate) active_connections: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub(crate) total_connections: Arc<dyn Fn() -> u64 + Send + Sync>,
    pub(crate) prefix: String,
}

pub async fn serve(
    addr: &str,
    csv_path: PathBuf,
    stats: Stats,
    prefix: &str,
) -> anyhow::Result<()> {
    let state = AppState {
        csv_path: Arc::new(Mutex::new(csv_path)),
        active_connections: stats.active_connections,
        total_connections: stats.total_connections,
        prefix: prefix.to_string(),
    };

    let app = Router::new()
        .route(&format!("{prefix}/"), get(serve_dashboard))
        .route(&format!("{prefix}"), get(serve_dashboard))
        .route(&format!("{prefix}/editor"), get(serve_editor))
        .route(&format!("{prefix}/editor/api/anchors"), get(get_anchors))
        .route(&format!("{prefix}/editor/api/save"), post(save_anchors))
        .with_state(state);

    println!("[admin] Listening on http://{addr}{prefix}/");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

pub(crate) fn read_csv(path: &PathBuf) -> anyhow::Result<Vec<AnchorPoint>> {
    let mut rdr = csv::Reader::from_path(path)?;
    let anchors = rdr
        .deserialize::<AnchorPoint>()
        .filter_map(|r| r.ok())
        .collect();
    Ok(anchors)
}

pub(crate) fn write_csv(path: &PathBuf, anchors: &[AnchorPoint]) -> anyhow::Result<()> {
    let mut wtr = csv::Writer::from_path(path)?;
    for anchor in anchors {
        wtr.serialize(anchor)?;
    }
    wtr.flush()?;
    Ok(())
}
