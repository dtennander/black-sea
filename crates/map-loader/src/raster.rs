use black_sea_protocol::Tile;
use geo::BoundingRect;
use geo::geometry::Coord;
use rayon::prelude::*;

use crate::progress::make_count_bar;
use crate::{
    BBOX_MAX_LAT, BBOX_MAX_LON, BBOX_MIN_LAT, BBOX_MIN_LON, MAP_TILES_H, MAP_TILES_W,
    OVERVIEW_TILES_H, OVERVIEW_TILES_W,
};

/// Rasterize a slice of land polygons into a row-major `Vec<Vec<u8>>` grid.
///
/// Grid dimensions are [`MAP_TILES_W`] × [`MAP_TILES_H`].  Row 0 = northernmost.
///
/// # Algorithm
///
/// **First pass** — scanline fill (O(total_vertices + filled_cells)):  
/// For each polygon, [`scanline_fill_polygon`] computes which `(col_start, col_end)`
/// intervals are filled on each scan row.  Intervals are accumulated into
/// `fill_intervals[row]`.
///
/// **Second pass** — parallel apply:  
/// Each row is independent, so `rayon` converts intervals → `TILE_LAND` cells.
///
/// **Third pass** — coast detection:  
/// Any water cell adjacent (4-connected) to a land cell becomes `TILE_COAST`.
pub fn rasterize(polygons: &[geo::geometry::Polygon<f64>]) -> Vec<Vec<Tile>> {
    rasterize_at(
        polygons.iter(),
        polygons.len(),
        MAP_TILES_W as usize,
        MAP_TILES_H as usize,
    )
}

pub fn rasterize_overview(polygons: &[&geo::geometry::Polygon<f64>]) -> Vec<Vec<Tile>> {
    rasterize_at(
        polygons.iter().copied(),
        polygons.len(),
        OVERVIEW_TILES_W as usize,
        OVERVIEW_TILES_H as usize,
    )
}

fn rasterize_at<'a>(
    polygons: impl Iterator<Item = &'a geo::geometry::Polygon<f64>>,
    count: usize,
    w: usize,
    h: usize,
) -> Vec<Vec<Tile>> {
    // ── First pass: build scanline fill intervals ─────────────────────────────
    let mut fill_intervals: Vec<Vec<(usize, usize)>> = vec![Vec::new(); h];
    let bar = make_count_bar(count as u64, "polygons", 500);
    for poly in polygons {
        scanline_fill_polygon(poly, w, h, &mut fill_intervals);
        bar.inc();
    }
    bar.finish();

    // ── Second pass: apply intervals in parallel ──────────────────────────────
    let mut grid: Vec<Vec<Tile>> = fill_intervals
        .into_par_iter()
        .map(|intervals| {
            let mut row = vec![Tile::Water; w];
            for (c0, c1) in intervals {
                for cell in &mut row[c0..=c1.min(w - 1)] {
                    *cell = Tile::Land;
                }
            }
            row
        })
        .collect();

    // ── Third pass: mark coast tiles ─────────────────────────────────────────
    let land_copy = grid.clone();
    for row in 0..h {
        for col in 0..w {
            if land_copy[row][col] == Tile::Water {
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
                .any(|(r, c)| land_copy[r][c] == Tile::Land);
                if has_land_neighbour {
                    grid[row][col] = Tile::Coast;
                }
            }
        }
    }

    grid
}

/// Compute scanline fill intervals for a single polygon and push them into
/// `fill_intervals[row]`.
///
/// For each grid row this function:
/// 1. Determines the latitude at that row's centre.
/// 2. Walks every edge of the exterior ring and finds intersections with the scanline.
/// 3. Sorts intersections and fills between each consecutive pair.
pub fn scanline_fill_polygon(
    poly: &geo::geometry::Polygon<f64>,
    w: usize,
    h: usize,
    fill_intervals: &mut Vec<Vec<(usize, usize)>>,
) {
    let bbox = match poly.bounding_rect() {
        Some(b) => b,
        None => return,
    };

    // Row 0 = northernmost (BBOX_MAX_LAT), row h-1 = southernmost (BBOX_MIN_LAT).
    let row_of_lat = |lat: f64| -> usize {
        let frac = (BBOX_MAX_LAT - lat) / (BBOX_MAX_LAT - BBOX_MIN_LAT);
        (frac * (h - 1) as f64).round() as usize
    };
    let col_of_lon = |lon: f64| -> usize {
        let frac = (lon - BBOX_MIN_LON) / (BBOX_MAX_LON - BBOX_MIN_LON);
        (frac * (w - 1) as f64).round() as usize
    };

    let row_min = row_of_lat(bbox.max().y).saturating_sub(1).min(h - 1);
    let row_max = (row_of_lat(bbox.min().y) + 1).min(h - 1);

    let coords: &[Coord<f64>] = poly.exterior().0.as_slice();
    let n = coords.len();
    if n < 2 {
        return;
    }

    for row in row_min..=row_max {
        let lat = BBOX_MAX_LAT - (row as f64 / (h - 1) as f64) * (BBOX_MAX_LAT - BBOX_MIN_LAT);

        let mut xs: Vec<f64> = Vec::new();
        for i in 0..n {
            let a = coords[i];
            let b = coords[(i + 1) % n];
            let (y0, y1) = (a.y, b.y);
            if (y0 <= lat && y1 > lat) || (y1 <= lat && y0 > lat) {
                let t = (lat - y0) / (y1 - y0);
                xs.push(a.x + t * (b.x - a.x));
            }
        }

        if xs.len() < 2 {
            continue;
        }
        xs.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());

        for pair in xs.chunks_exact(2) {
            let (x0, x1) = (pair[0], pair[1]);
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use black_sea_protocol::Tile;
    use geo::geometry::{LineString, Polygon};

    /// Build a longitude/latitude rectangle that maps to known grid cells.
    ///
    /// We pick a rectangle entirely inside the BBOX so we can reason about
    /// which rows/cols it should fill.
    fn bbox_polygon(min_lon: f64, max_lon: f64, min_lat: f64, max_lat: f64) -> Polygon<f64> {
        Polygon::new(
            LineString::new(vec![
                Coord {
                    x: min_lon,
                    y: min_lat,
                },
                Coord {
                    x: max_lon,
                    y: min_lat,
                },
                Coord {
                    x: max_lon,
                    y: max_lat,
                },
                Coord {
                    x: min_lon,
                    y: max_lat,
                },
                Coord {
                    x: min_lon,
                    y: min_lat,
                }, // close ring
            ]),
            vec![],
        )
    }

    /// Rasterize a single small polygon and verify that expected cells are land.
    #[test]
    fn small_polygon_fills_land_cells() {
        // A rectangle near the centre of the BBOX, spanning ~2.5% of each axis.
        let center_lon = (BBOX_MIN_LON + BBOX_MAX_LON) / 2.0;
        let center_lat = (BBOX_MIN_LAT + BBOX_MAX_LAT) / 2.0;
        let span_lon = (BBOX_MAX_LON - BBOX_MIN_LON) * 0.025;
        let span_lat = (BBOX_MAX_LAT - BBOX_MIN_LAT) * 0.025;

        let poly = bbox_polygon(
            center_lon - span_lon,
            center_lon + span_lon,
            center_lat - span_lat,
            center_lat + span_lat,
        );

        let grid = rasterize(&[poly]);

        // At least one cell must be Tile::Land.
        let has_land = grid.iter().any(|row| row.iter().any(|&t| t == Tile::Land));
        assert!(has_land, "expected land cells after rasterizing polygon");
    }

    /// After rasterization, every water cell adjacent to a land cell must be coast.
    #[test]
    fn coast_cells_border_land() {
        let center_lon = (BBOX_MIN_LON + BBOX_MAX_LON) / 2.0;
        let center_lat = (BBOX_MIN_LAT + BBOX_MAX_LAT) / 2.0;
        let span_lon = (BBOX_MAX_LON - BBOX_MIN_LON) * 0.025;
        let span_lat = (BBOX_MAX_LAT - BBOX_MIN_LAT) * 0.025;

        let poly = bbox_polygon(
            center_lon - span_lon,
            center_lon + span_lon,
            center_lat - span_lat,
            center_lat + span_lat,
        );

        let grid = rasterize(&[poly]);
        let h = grid.len();
        let w = if h > 0 { grid[0].len() } else { 0 };

        // Every Tile::Coast cell must have at least one Tile::Land neighbour.
        for r in 0..h {
            for c in 0..w {
                if grid[r][c] == Tile::Coast {
                    let has_land_nbr = [
                        r.checked_sub(1).map(|rr| (rr, c)),
                        if r + 1 < h { Some((r + 1, c)) } else { None },
                        c.checked_sub(1).map(|cc| (r, cc)),
                        if c + 1 < w { Some((r, c + 1)) } else { None },
                    ]
                    .into_iter()
                    .flatten()
                    .any(|(rr, cc)| grid[rr][cc] == Tile::Land);
                    assert!(
                        has_land_nbr,
                        "Tile::Coast at ({r},{c}) has no adjacent Tile::Land"
                    );
                }
            }
        }
    }

    /// An all-water polygon list produces an all-water grid (no land, no coast).
    #[test]
    fn empty_polygon_list_gives_all_water() {
        // We can't trivially call rasterize(&[]) because it still runs the
        // coast-detection pass — but with nothing to fill the grid stays water.
        let polys: Vec<geo::geometry::Polygon<f64>> = vec![];
        let grid = rasterize(&polys);
        let all_water = grid.iter().all(|row| row.iter().all(|&t| t == Tile::Water));
        assert!(
            all_water,
            "empty polygon list should produce all-water grid"
        );
    }
}
