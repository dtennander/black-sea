//! World geometry: the WGS-84 bounding box, tile grid sizes, and conversion
//! between geographic coordinates and tile-space.
//!
//! These constants define the protocol-level contract for what "position" means.
//! Both the server's map loader and any authoring tools (e.g. anchor editor)
//! must agree on them.

/// Bounding box for the Stockholm inner/mid archipelago (WGS-84).
pub const BBOX_MIN_LAT: f64 = 58.80;
pub const BBOX_MAX_LAT: f64 = 59.80;
pub const BBOX_MIN_LON: f64 = 17.50;
pub const BBOX_MAX_LON: f64 = 20.00;

/// Full map size in tiles.
pub const MAP_TILES_W: u32 = 8500;
pub const MAP_TILES_H: u32 = 5500;

/// Overview map size in tiles (1/20th of the full map).
pub const OVERVIEW_TILES_W: u32 = 425;
pub const OVERVIEW_TILES_H: u32 = 275;

/// Chunk size the server advertises (square tiles).
pub const CHUNK_SIZE: u32 = 50;

/// Approximate metres per tile.
pub const METRES_PER_TILE: f32 = 20.0;

/// Convert WGS-84 (latitude, longitude) in degrees to tile-space (x, y).
///
/// Row 0 (y = 0) is the northernmost row; y grows southward.
/// Column 0 (x = 0) is the westernmost column; x grows eastward.
///
/// The result is not clamped — callers that need a point strictly inside the
/// grid should check the returned coordinates.
pub fn lat_lon_to_tile(lat: f64, lon: f64) -> (f32, f32) {
    let x = (lon - BBOX_MIN_LON) / (BBOX_MAX_LON - BBOX_MIN_LON) * MAP_TILES_W as f64;
    let y = (BBOX_MAX_LAT - lat) / (BBOX_MAX_LAT - BBOX_MIN_LAT) * MAP_TILES_H as f64;
    (x as f32, y as f32)
}

/// Convert tile-space (x, y) back to WGS-84 (latitude, longitude) in degrees.
pub fn tile_to_lat_lon(x: f32, y: f32) -> (f64, f64) {
    let lon = BBOX_MIN_LON + (x as f64 / MAP_TILES_W as f64) * (BBOX_MAX_LON - BBOX_MIN_LON);
    let lat = BBOX_MAX_LAT - (y as f64 / MAP_TILES_H as f64) * (BBOX_MAX_LAT - BBOX_MIN_LAT);
    (lat, lon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_center() {
        let lat = (BBOX_MIN_LAT + BBOX_MAX_LAT) / 2.0;
        let lon = (BBOX_MIN_LON + BBOX_MAX_LON) / 2.0;
        let (x, y) = lat_lon_to_tile(lat, lon);
        let (lat2, lon2) = tile_to_lat_lon(x, y);
        assert!((lat - lat2).abs() < 1e-3);
        assert!((lon - lon2).abs() < 1e-3);
    }

    #[test]
    fn corners() {
        let (x, y) = lat_lon_to_tile(BBOX_MAX_LAT, BBOX_MIN_LON);
        assert!(x.abs() < 0.01 && y.abs() < 0.01);
        let (x, y) = lat_lon_to_tile(BBOX_MIN_LAT, BBOX_MAX_LON);
        assert!((x - MAP_TILES_W as f32).abs() < 0.01);
        assert!((y - MAP_TILES_H as f32).abs() < 0.01);
    }
}
