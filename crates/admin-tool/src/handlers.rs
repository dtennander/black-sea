use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
};

use crate::router::{AnchorLatLon, AnchorPoint, AppState};

pub(crate) async fn serve_dashboard(State(state): State<AppState>) -> impl IntoResponse {
    let active = (state.active_connections)();
    let total = (state.total_connections)();
    let anchors = {
        let path = state.csv_path.lock().unwrap().clone();
        crate::router::read_csv(&path).unwrap_or_default()
    };

    let anchor_rows: String = anchors
        .iter()
        .map(|a| {
            let note = if a.note.is_empty() {
                "—".to_string()
            } else {
                a.note.clone()
            };
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                html_escape(&a.name),
                a.x,
                a.y,
                html_escape(&note)
            )
        })
        .collect();

    let html = include_str!("dashboard.html")
        .replace("{ACTIVE}", &active.to_string())
        .replace("{TOTAL}", &total.to_string())
        .replace("{ANCHOR_COUNT}", &anchors.len().to_string())
        .replace("{PREFIX}", &state.prefix)
        .replace("{ANCHOR_ROWS}", &anchor_rows);

    Html(html)
}

pub(crate) async fn serve_editor(State(state): State<AppState>) -> impl IntoResponse {
    let html = include_str!("editor.html").replace("{PREFIX}", &state.prefix);
    Html(html)
}

pub(crate) async fn get_anchors(State(state): State<AppState>) -> impl IntoResponse {
    let path = state.csv_path.lock().unwrap().clone();
    let anchors = crate::router::read_csv(&path).unwrap_or_default();
    let response: Vec<AnchorLatLon> = anchors
        .into_iter()
        .map(|a| {
            let (lat, lon) = black_sea_protocol::coords::tile_to_lat_lon(a.x as f32, a.y as f32);
            AnchorLatLon {
                name: a.name,
                lat,
                lon,
                note: a.note,
            }
        })
        .collect();
    Json(response)
}

pub(crate) async fn save_anchors(
    State(state): State<AppState>,
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

    let path = state.csv_path.lock().unwrap().clone();
    match crate::router::write_csv(&path, &anchors) {
        Ok(_) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
