use std::collections::HashMap;

use black_sea_protocol::{MapGrid, Position};
use rand::Rng;

use crate::handler::BoatEntry;

/// Minimum tile distance between any two spawned boats.
const MIN_SEPARATION: f32 = 5.0;

/// Maximum placement attempts per search radius before doubling the radius.
const MAX_PLACEMENT_ATTEMPTS: usize = 1000;

/// World-tile coordinates of the default spawn anchor (near Sandhamn).
const SPAWN_ANCHOR_X: f32 = 4590.0;
const SPAWN_ANCHOR_Y: f32 = 2728.0;

/// Pick a random water position near Sandhamn, respecting [`MIN_SEPARATION`].
///
/// The search radius doubles on each retry pass until it covers the full map.
/// Returns `None` only when the world is completely full (extremely unlikely).
pub fn find_free_position(boats: &HashMap<u64, BoatEntry>, map: &MapGrid) -> Option<Position> {
    let mut rng = rand::rng();
    let mut radius = 10.0_f32;
    let max_radius = (map.width.max(map.height) as f32) / 2.0;

    while radius <= max_radius {
        for _ in 0..MAX_PLACEMENT_ATTEMPTS {
            let candidate = Position {
                x: (SPAWN_ANCHOR_X + rng.random_range(-radius..radius))
                    .clamp(0.0, map.width as f32 - 1.0),
                y: (SPAWN_ANCHOR_Y + rng.random_range(-radius..radius))
                    .clamp(0.0, map.height as f32 - 1.0),
            };
            if !map.is_passable(&candidate) {
                continue;
            }
            let too_close = boats.values().any(|entry| {
                let dx = entry.position.x - candidate.x;
                let dy = entry.position.y - candidate.y;
                (dx * dx + dy * dy).sqrt() < MIN_SEPARATION
            });
            if !too_close {
                return Some(candidate);
            }
        }
        radius *= 2.0;
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use black_sea_protocol::Tile;

    fn all_water_map() -> MapGrid {
        MapGrid {
            grid: vec![vec![Tile::Water; 100]; 100],
            width: 100,
            height: 100,
            chunk_size: 10,
        }
    }

    fn all_land_map() -> MapGrid {
        MapGrid {
            grid: vec![vec![Tile::Land; 100]; 100],
            width: 100,
            height: 100,
            chunk_size: 10,
        }
    }

    #[test]
    fn finds_position_on_empty_water_map() {
        let map = all_water_map();
        let result = find_free_position(&HashMap::new(), &map);
        // There are no obstacles, so we always find a spot.
        assert!(result.is_some());
    }

    #[test]
    fn returns_none_on_all_land_map() {
        let map = all_land_map();
        let result = find_free_position(&HashMap::new(), &map);
        assert!(
            result.is_none(),
            "should not find a passable position on solid land"
        );
    }

    #[test]
    fn respects_minimum_separation() {
        let map = all_water_map();
        // Place one boat at the centre of our small map.
        let mut existing: HashMap<u64, BoatEntry> = HashMap::new();
        existing.insert(
            0,
            BoatEntry {
                position: Position { x: 50.0, y: 50.0 },
                name: "boat0".to_string(),
            },
        );

        // The search should still find an empty spot somewhere on the map.
        let result = find_free_position(&existing, &map);
        assert!(
            result.is_some(),
            "should find a free spot with only one existing boat"
        );

        if let Some(pos) = result {
            let dx = 50.0_f32 - pos.x;
            let dy = 50.0_f32 - pos.y;
            let dist = (dx * dx + dy * dy).sqrt();
            assert!(
                dist >= MIN_SEPARATION,
                "spawned position too close to existing boat (dist={dist:.2})"
            );
        }
    }
}
