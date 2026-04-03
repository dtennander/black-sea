use serde_repr::{Deserialize_repr, Serialize_repr};

/// Tile type for every cell in the map grid.
///
/// Serialized as a `u8` (via `serde_repr`) so the wire format is identical
/// to the former bare-constant encoding: `Water=0`, `Coast=1`, `Land=2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum Tile {
    /// Open water — passable by boats.
    Water = 0,
    /// Coastline / shallow water — adjacent to land. Not passable.
    Coast = 1,
    /// Solid land — not passable.
    Land = 2,
}

/// The rasterized world map.
///
/// The grid is stored row-major with **row 0 = northernmost row** (`grid[y][x]`).
/// All out-of-bounds accesses silently return [`Tile::Water`].
#[derive(Debug, Clone)]
pub struct MapGrid {
    /// Raw tile data: `grid[row][col]` = `grid[y][x]`.
    pub grid: Vec<Vec<Tile>>,
    pub width: u32,
    pub height: u32,
    pub chunk_size: u32,
}

impl MapGrid {
    /// Return the tile at grid coordinates `(col, row)`.  Out-of-bounds → [`Tile::Water`].
    pub fn tile_at(&self, col: u32, row: u32) -> Tile {
        self.grid
            .get(row as usize)
            .and_then(|r| r.get(col as usize))
            .copied()
            .unwrap_or(Tile::Water)
    }

    /// Return the tile at a floating-point [`crate::Position`] (clamped to grid bounds).
    pub fn tile_at_pos(&self, pos: &crate::Position) -> Tile {
        let col = pos.x.clamp(0.0, self.width as f32 - 1.0) as u32;
        let row = pos.y.clamp(0.0, self.height as f32 - 1.0) as u32;
        self.tile_at(col, row)
    }

    /// Return `true` if a boat may occupy `pos` (i.e. the tile is open water).
    pub fn is_passable(&self, pos: &crate::Position) -> bool {
        self.tile_at_pos(pos) == Tile::Water
    }

    /// Extract a single chunk as a flat `Vec<Tile>` in row-major order (`chunk_size²` elements).
    ///
    /// Rows outside the grid boundaries are filled with [`Tile::Water`].
    pub fn chunk_data(&self, chunk_x: u32, chunk_y: u32) -> Vec<Tile> {
        let cs = self.chunk_size as usize;
        let origin_col = (chunk_x * self.chunk_size) as usize;
        let origin_row = (chunk_y * self.chunk_size) as usize;
        let mut data = Vec::with_capacity(cs * cs);
        for row in origin_row..origin_row + cs {
            for col in origin_col..origin_col + cs {
                let tile = self
                    .grid
                    .get(row)
                    .and_then(|r| r.get(col))
                    .copied()
                    .unwrap_or(Tile::Water);
                data.push(tile);
            }
        }
        data
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Position;

    fn small_grid() -> MapGrid {
        // 4×4 grid:
        //   W W W W
        //   W L L W
        //   W L L W
        //   W W W W
        // (row 0 = top/north)
        let mut grid = vec![vec![Tile::Water; 4]; 4];
        grid[1][1] = Tile::Land;
        grid[1][2] = Tile::Land;
        grid[2][1] = Tile::Land;
        grid[2][2] = Tile::Land;
        MapGrid {
            grid,
            width: 4,
            height: 4,
            chunk_size: 2,
        }
    }

    #[test]
    fn tile_at_water() {
        let g = small_grid();
        assert_eq!(g.tile_at(0, 0), Tile::Water);
        assert_eq!(g.tile_at(3, 3), Tile::Water);
    }

    #[test]
    fn tile_at_land() {
        let g = small_grid();
        assert_eq!(g.tile_at(1, 1), Tile::Land);
        assert_eq!(g.tile_at(2, 2), Tile::Land);
    }

    #[test]
    fn tile_at_out_of_bounds_returns_water() {
        let g = small_grid();
        assert_eq!(g.tile_at(99, 99), Tile::Water);
        assert_eq!(g.tile_at(u32::MAX, u32::MAX), Tile::Water);
    }

    #[test]
    fn tile_at_pos_clamps() {
        let g = small_grid();
        // Negative floats clamp to 0 → water
        assert_eq!(g.tile_at_pos(&Position { x: -5.0, y: -5.0 }), Tile::Water);
        // Beyond map clamps to edge → water (edge is col=3, row=3)
        assert_eq!(g.tile_at_pos(&Position { x: 999.0, y: 999.0 }), Tile::Water);
        // Inside land block
        assert_eq!(g.tile_at_pos(&Position { x: 1.0, y: 1.0 }), Tile::Land);
    }

    #[test]
    fn is_passable() {
        let g = small_grid();
        assert!(g.is_passable(&Position { x: 0.0, y: 0.0 }));
        assert!(!g.is_passable(&Position { x: 1.0, y: 1.0 }));
    }

    #[test]
    fn is_passable_coast_blocked() {
        let mut g = small_grid();
        g.grid[0][0] = Tile::Coast;
        assert!(!g.is_passable(&Position { x: 0.0, y: 0.0 }));
    }

    #[test]
    fn chunk_data_out_of_bounds_filled_with_water() {
        let g = small_grid();
        // Chunk (5,5) is entirely out of bounds
        let data = g.chunk_data(5, 5);
        assert_eq!(data, vec![Tile::Water; 4]);
    }
}
