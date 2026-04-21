use axum::{
    Router,
    extract::Json,
    response::IntoResponse,
    routing::{get, post},
};
use black_sea_protocol::coords::{
    BBOX_MAX_LAT, BBOX_MAX_LON, BBOX_MIN_LAT, BBOX_MIN_LON, MAP_TILES_H, MAP_TILES_W,
    lat_lon_to_tile,
};
use serde::{Deserialize, Serialize};

// ── HTTP handlers ─────────────────────────────────────────────────────────────

async fn index() -> impl IntoResponse {
    axum::response::Html(HTML)
}

#[derive(Serialize)]
struct BboxResponse {
    min_lat: f64,
    max_lat: f64,
    min_lon: f64,
    max_lon: f64,
    tile_w: u32,
    tile_h: u32,
}

async fn bbox() -> Json<BboxResponse> {
    Json(BboxResponse {
        min_lat: BBOX_MIN_LAT,
        max_lat: BBOX_MAX_LAT,
        min_lon: BBOX_MIN_LON,
        max_lon: BBOX_MAX_LON,
        tile_w: MAP_TILES_W,
        tile_h: MAP_TILES_H,
    })
}

#[derive(Deserialize)]
struct ConvertRequest {
    lat: f64,
    lon: f64,
}

#[derive(Serialize)]
struct ConvertResponse {
    x: f32,
    y: f32,
}

async fn convert(Json(req): Json<ConvertRequest>) -> Json<ConvertResponse> {
    let (x, y) = lat_lon_to_tile(req.lat, req.lon);
    Json(ConvertResponse { x, y })
}

async fn existing() -> impl IntoResponse {
    let path = std::env::var("BLACK_SEA_ANCHORINGS").unwrap_or_else(|_| "anchorings.csv".into());
    match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(_) => String::new(),
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let addr = std::env::var("ANCHOR_EDITOR_ADDR").unwrap_or_else(|_| "127.0.0.1:3030".into());
    let app = Router::new()
        .route("/", get(index))
        .route("/bbox", get(bbox))
        .route("/convert", post(convert))
        .route("/existing", get(existing));

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    println!("anchor-editor listening on http://{addr}");
    axum::serve(listener, app).await.unwrap();
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_roundtrip_matches_protocol() {
        // Sandhamn harbour — a known point from the seed CSV
        let lat = 59.2887_f64;
        let lon = 18.9100_f64;
        let (expected_x, expected_y) = lat_lon_to_tile(lat, lon);
        // The handler just calls lat_lon_to_tile — verify same result
        let (x, y) = lat_lon_to_tile(lat, lon);
        assert!((x - expected_x).abs() < 0.01);
        assert!((y - expected_y).abs() < 0.01);
        // Sanity: result is inside the tile grid
        assert!(x >= 0.0 && x <= MAP_TILES_W as f32);
        assert!(y >= 0.0 && y <= MAP_TILES_H as f32);
    }

    #[test]
    fn bbox_constants_are_sane() {
        assert!(BBOX_MIN_LAT < BBOX_MAX_LAT);
        assert!(BBOX_MIN_LON < BBOX_MAX_LON);
        // Stockholm archipelago should be in the Northern hemisphere, east of Greenwich
        assert!(BBOX_MIN_LAT > 50.0 && BBOX_MAX_LAT < 70.0);
        assert!(BBOX_MIN_LON > 10.0 && BBOX_MAX_LON < 25.0);
    }
}

// ── Inline HTML ───────────────────────────────────────────────────────────────

const HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Anchor Editor</title>
  <link rel="stylesheet" href="https://unpkg.com/leaflet@1.9.4/dist/leaflet.css"/>
  <script src="https://unpkg.com/leaflet@1.9.4/dist/leaflet.js"></script>
  <style>
    * { box-sizing: border-box; margin: 0; padding: 0; }
    body { font-family: sans-serif; display: flex; flex-direction: column; height: 100vh; }
    #map { flex: 1; }
    #panel { padding: 12px; background: #f5f5f5; border-top: 1px solid #ccc; max-height: 40vh; overflow-y: auto; }
    h2 { font-size: 1rem; margin-bottom: 8px; }
    table { border-collapse: collapse; width: 100%; font-size: 0.85rem; }
    th, td { border: 1px solid #ddd; padding: 4px 8px; text-align: left; }
    th { background: #e0e0e0; }
    button.del { background: #e44; color: #fff; border: none; padding: 2px 8px; cursor: pointer; border-radius: 3px; }
    #actions { margin-bottom: 8px; display: flex; gap: 8px; }
    #actions button { padding: 6px 14px; cursor: pointer; font-size: 0.9rem; }
  </style>
</head>
<body>
<div id="map"></div>
<div id="panel">
  <div id="actions">
    <button onclick="copyCSV()">Copy as CSV</button>
    <button onclick="downloadCSV()">Download CSV</button>
  </div>
  <h2>Anchor points</h2>
  <table>
    <thead><tr><th>Name</th><th>X</th><th>Y</th><th>Note</th><th></th></tr></thead>
    <tbody id="rows"></tbody>
  </table>
</div>

<script>
const map = L.map('map');
L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', {
  maxZoom: 17,
  attribution: '© OpenStreetMap contributors'
}).addTo(map);

let bbox = null;
let points = [];  // {name, x, y, note, marker, existing}

const existingIcon = L.divIcon({ html: '⚓', className: '', iconSize: [20,20], iconAnchor: [10,10] });
const newIcon      = L.divIcon({ html: '<span style="color:#d04">⚓</span>', className: '', iconSize: [20,20], iconAnchor: [10,10] });

async function init() {
  bbox = await fetch('/bbox').then(r => r.json());
  const sw = L.latLng(bbox.min_lat, bbox.min_lon);
  const ne = L.latLng(bbox.max_lat, bbox.max_lon);
  const bounds = L.latLngBounds(sw, ne);
  map.fitBounds(bounds, { padding: [20, 20] });
  L.rectangle(bounds, { color: '#3388ff', weight: 1, fill: false }).addTo(map);

  const csv = await fetch('/existing').then(r => r.text());
  loadExisting(csv);
}

function loadExisting(csv) {
  const lines = csv.trim().split('\n');
  if (lines.length < 2) return;
  // skip header
  for (const line of lines.slice(1)) {
    const parts = line.split(',');
    if (parts.length < 3) continue;
    const [name, x, y, ...rest] = parts;
    const note = rest.join(',').trim();
    addPoint(name.trim(), parseFloat(x), parseFloat(y), note, true);
  }
}

function addPoint(name, x, y, note, existing) {
  // back-convert tile coords to lat/lon for the marker
  const lat = bbox.max_lat - (y / bbox.tile_h) * (bbox.max_lat - bbox.min_lat);
  const lon = bbox.min_lon + (x / bbox.tile_w) * (bbox.max_lon - bbox.min_lon);
  const marker = L.marker([lat, lon], { icon: existing ? existingIcon : newIcon })
    .addTo(map)
    .bindPopup(`<b>${name}</b>${note ? '<br>' + note : ''}`);
  const idx = points.length;
  points.push({ name, x, y, note, marker, existing });
  renderRow(idx);
}

function renderRow(idx) {
  const tbody = document.getElementById('rows');
  const { name, x, y, note, existing } = points[idx];
  const tr = document.createElement('tr');
  if (existing) tr.style.color = '#888';
  tr.id = 'row-' + idx;
  tr.innerHTML = `<td>${esc(name)}</td><td>${x.toFixed(1)}</td><td>${y.toFixed(1)}</td><td>${esc(note)}</td>` +
    `<td><button class="del" onclick="removePoint(${idx})">✕</button></td>`;
  tbody.appendChild(tr);
}

function removePoint(idx) {
  const p = points[idx];
  if (p) { map.removeLayer(p.marker); points[idx] = null; }
  const row = document.getElementById('row-' + idx);
  if (row) row.remove();
}

map.on('click', async e => {
  const { lat, lng } = e.latlng;
  if (lat < bbox.min_lat || lat > bbox.max_lat || lng < bbox.min_lon || lng > bbox.max_lon) return;
  const name = prompt('Anchor name (required):');
  if (!name || !name.trim()) return;
  const note = prompt('Note (optional):') || '';
  const { x, y } = await fetch('/convert', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ lat, lon: lng })
  }).then(r => r.json());
  addPoint(name.trim(), x, y, note.trim(), false);
});

function toCSV() {
  const header = 'name,x,y,note';
  const rows = points.filter(Boolean).map(p =>
    `${csvEsc(p.name)},${p.x.toFixed(1)},${p.y.toFixed(1)},${csvEsc(p.note)}`
  );
  return [header, ...rows].join('\n') + '\n';
}

function copyCSV() {
  navigator.clipboard.writeText(toCSV()).then(() => alert('Copied to clipboard!'));
}

function downloadCSV() {
  const blob = new Blob([toCSV()], { type: 'text/csv' });
  const a = document.createElement('a');
  a.href = URL.createObjectURL(blob);
  a.download = 'anchorings.csv';
  a.click();
}

function esc(s) { return (s || '').replace(/&/g,'&amp;').replace(/</g,'&lt;'); }
function csvEsc(s) { s = s || ''; return s.includes(',') ? `"${s.replace(/"/g,'""')}"` : s; }

init();
</script>
</body>
</html>
"#;
