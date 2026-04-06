pub mod download;
pub mod parser;
pub mod progress;
pub mod raster;

use anyhow::Result;
use black_sea_protocol::MapGrid;
use rayon::prelude::*;

use download::download_land_polygons;
use parser::parse_shapefile_from_zip;
use progress::make_count_bar;

/// Bounding box for the Stockholm inner/mid archipelago (WGS-84).
pub const BBOX_MIN_LAT: f64 = 58.80;
pub const BBOX_MAX_LAT: f64 = 59.80;
pub const BBOX_MIN_LON: f64 = 17.50;
pub const BBOX_MAX_LON: f64 = 20.00;

/// Total map size in tiles.
pub const MAP_TILES_W: u32 = 8500;
pub const MAP_TILES_H: u32 = 5500;

/// Overview map size in tiles (1/20th of the full map).
pub const OVERVIEW_TILES_W: u32 = 425;
pub const OVERVIEW_TILES_H: u32 = 275;

/// Chunk size the server advertises (square tiles).
pub const CHUNK_SIZE: u32 = 50;

/// Approximate metres per tile — passed to clients via [`GameEvent::WorldInfoEvent`].
pub const METRES_PER_TILE: f32 = 20.0;

/// Half a tile in degrees — detail finer than this is invisible in the output.
/// At 8500 tiles over ~170 km each tile ≈ 20 m; `SIMPLIFY_EPSILON` ≈ 0.001°.
const SIMPLIFY_EPSILON: f64 = 0.001;

/// Minimum polygon area (in degrees²) to include in the overview map.
/// At 425×275 tiles over 2.5°×1°, one overview pixel ≈ 0.000024 deg².
/// 0.0004 deg² ≈ 16 overview pixels — filters small islets while keeping
/// islands with recognizable shape.
const OVERVIEW_MIN_AREA_DEG2: f64 = 0.0004;

/// Download OSM land polygons and rasterize them into a full [`MapGrid`] and a
/// low-resolution overview [`MapGrid`].
///
/// Uses an ETag-based disk cache under `./osm-cache/` (or `BLACK_SEA_CACHE_DIR`).
pub fn load_map() -> Result<(MapGrid, MapGrid)> {
    println!("[map] Downloading OSM land polygons...");
    let zip_bytes = download_land_polygons()?;

    println!("[map] Parsing Shapefile from zip...");
    let polygons = parse_shapefile_from_zip(&zip_bytes)?;

    // Simplify polygons to remove sub-tile detail — drastically reduces vertex
    // counts on complex coastlines before the scanline rasterizer runs.
    println!(
        "[map] Simplifying {} polygons (epsilon={SIMPLIFY_EPSILON})...",
        polygons.len()
    );
    let bar = make_count_bar(polygons.len() as u64, "polygons simplified", 2000);
    let polygons: Vec<geo::geometry::Polygon<f64>> = polygons
        .into_par_iter()
        .map(|p| {
            use geo::Simplify;
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
        "[map] Rasterizing {} polygons to {}×{} grid...",
        polygons.len(),
        MAP_TILES_W,
        MAP_TILES_H
    );
    let grid = raster::rasterize(&polygons);

    let overview_polygons: Vec<&geo::geometry::Polygon<f64>> = {
        use geo::Area;
        polygons
            .iter()
            .filter(|p| p.unsigned_area() >= OVERVIEW_MIN_AREA_DEG2)
            .collect()
    };
    println!(
        "[map] Rasterizing overview {}×{} grid ({} polygons after filtering small islands)...",
        OVERVIEW_TILES_W, OVERVIEW_TILES_H, overview_polygons.len()
    );
    let overview_grid = raster::rasterize_overview(&overview_polygons);

    let full = MapGrid {
        grid,
        width: MAP_TILES_W,
        height: MAP_TILES_H,
        chunk_size: CHUNK_SIZE,
    };
    let overview = MapGrid {
        grid: overview_grid,
        width: OVERVIEW_TILES_W,
        height: OVERVIEW_TILES_H,
        chunk_size: OVERVIEW_TILES_H, // single chunk — not used for overview
    };
    Ok((full, overview))
}
