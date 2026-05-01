use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
};
use clap::Parser;
use serde::{Deserialize, Serialize};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "anchorings.csv")]
    csv: PathBuf,
    #[arg(long, default_value_t = 3000)]
    port: u16,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct AnchorPoint {
    name: String,
    x: u32,
    y: u32,
    note: String,
}

/// What the browser sends and receives: lat/lon in WGS-84.
/// The server converts to/from tile space internally.
#[derive(Debug, Serialize, Deserialize)]
struct AnchorLatLon {
    name: String,
    lat: f64,
    lon: f64,
    note: String,
}

type AppState = Arc<Mutex<PathBuf>>;

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let state: AppState = Arc::new(Mutex::new(args.csv));

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/api/anchors", get(get_anchors))
        .route("/api/save", post(save_anchors))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", args.port);
    println!("Admin tool listening on http://localhost:{}", args.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn serve_index() -> impl IntoResponse {
    Html(include_str!("index.html"))
}

async fn get_anchors(State(csv_path): State<AppState>) -> impl IntoResponse {
    let path = csv_path.lock().unwrap().clone();
    let anchors = read_csv(&path).unwrap_or_default();
    // Convert tile space → lat/lon so the browser only ever sees geographic coords.
    let response: Vec<AnchorLatLon> = anchors
        .into_iter()
        .map(|a| {
            let (lat, lon) = black_sea_protocol::coords::tile_to_lat_lon(a.x as f32, a.y as f32);
            AnchorLatLon { name: a.name, lat, lon, note: a.note }
        })
        .collect();
    Json(response)
}

async fn save_anchors(
    State(csv_path): State<AppState>,
    Json(inputs): Json<Vec<AnchorLatLon>>,
) -> impl IntoResponse {
    let anchors: Vec<AnchorPoint> = inputs
        .into_iter()
        .map(|a| {
            let (x, y) = black_sea_protocol::coords::lat_lon_to_tile(a.lat, a.lon);
            AnchorPoint {
                name: a.name,
                x: x.round() as u32,
                y: y.round() as u32,
                note: a.note,
            }
        })
        .collect();

    let path = csv_path.lock().unwrap().clone();
    match write_csv(&path, &anchors) {
        Ok(_) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn read_csv(path: &PathBuf) -> anyhow::Result<Vec<AnchorPoint>> {
    let mut rdr = csv::Reader::from_path(path)?;
    let anchors = rdr
        .deserialize::<AnchorPoint>()
        .filter_map(|r| r.ok())
        .collect();
    Ok(anchors)
}

fn write_csv(path: &PathBuf, anchors: &[AnchorPoint]) -> anyhow::Result<()> {
    let mut wtr = csv::Writer::from_path(path)?;
    for anchor in anchors {
        wtr.serialize(anchor)?;
    }
    wtr.flush()?;
    Ok(())
}
