use black_sea_protocol::Tile;
use black_sea_protocol::coords::{MAP_TILES_H, MAP_TILES_W};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Block;
use ratatui::widgets::Paragraph;
use ratatui::widgets::canvas::{Canvas, Context};

use crate::app::{
    App, BUBBLE_OFFSET, BUBBLE_TTL, Direction, PAGE_TILES_H, PAGE_TILES_W, UpdateStatus,
};

// ── Boat glyphs ───────────────────────────────────────────────────────────────

/// Return a list of `(x_offset, y_offset, glyph)` triples for a boat.
///
/// The first element is always the hull anchored at `(0, 0)` in canvas space.
/// Additional elements are sail/mast parts offset from the hull.
pub fn boat_glyphs(
    name: &str,
    dir: Direction,
    row_step: f64,
    col_step: f64,
) -> Vec<(f64, f64, String)> {
    match dir {
        Direction::Right => {
            let hull = format!("/{}/", name);
            let sail_x = ((hull.len() as f64 - 3.0) / 2.0).max(0.0) * col_step;
            vec![(0.0, 0.0, hull), (sail_x, row_step, "/|)".to_string())]
        }
        Direction::Left => {
            let hull = format!("\\{}\\", name);
            let sail_x = ((hull.len() as f64 - 3.0) / 2.0).max(0.0) * col_step;
            vec![(0.0, 0.0, hull), (sail_x, row_step, "(|\\".to_string())]
        }
        Direction::Up => vec![
            (col_step * 2.0, row_step * 2.0, "^".to_string()),
            (col_step, row_step, "/|\\".to_string()),
            (col_step, 0.0, "\\ /".to_string()),
        ],
        Direction::Down => vec![
            (col_step, row_step, "/ \\".to_string()),
            (col_step, 0.0, "\\|/".to_string()),
            (col_step * 2.0, -row_step, "V".to_string()),
        ],
    }
}

// ── Coast character selection ─────────────────────────────────────────────────

/// Return the ASCII character for a coastline tile at world coordinates `(wx, wy)`.
///
/// Only cardinal neighbours are examined; diagonals are ignored.
pub fn coast_char(app: &App, wx: u32, wy: u32) -> &'static str {
    let solid = |x: i32, y: i32| -> bool {
        if x < 0 || y < 0 {
            return false;
        }
        matches!(app.tile_at_world(x as u32, y as u32), Some(t) if t != Tile::Water)
    };

    let x = wx as i32;
    let y = wy as i32;

    let n = solid(x, y - 1);
    let s = solid(x, y + 1);
    let w = solid(x - 1, y);
    let e = solid(x + 1, y);

    match (n, s, w, e) {
        // Straight runs: character matches direction of travel along the coastline
        (false, false, true, true) => "-", // W+E solid → walking east-west
        (false, false, true, false) => "-", // W only
        (false, false, false, true) => "-", // E only
        (true, true, false, false) => "|", // N+S solid → walking north-south
        (true, false, false, false) => "|", // N only
        (false, true, false, false) => "|", // S only
        // Corners
        (false, true, false, true) => "/",  // SW corner
        (true, false, true, false) => "/",  // NE corner
        (false, true, true, false) => "\\", // SE corner
        (true, false, false, true) => "\\", // NW corner
        // T-junctions: continue the dominant axis
        (true, true, true, false) => "|", // N+S+W → north-south run
        (true, true, false, true) => "|", // N+S+E → north-south run
        (true, false, true, true) => "-", // N+W+E → east-west run
        (false, true, true, true) => "-", // S+W+E → east-west run
        // Cross
        (true, true, true, true) => "+",
        // Isolated — guard in draw_coastline prevents this during rendering
        _ => " ",
    }
}

// ── Main render function ──────────────────────────────────────────────────────

pub fn render(frame: &mut Frame, app: &App) {
    if app.show_map_overview {
        render_map_overview(frame, app);
        return;
    }

    if let UpdateStatus::Incompatible { server_version } = &app.update_status {
        let [world_area, warn_area, input_area] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .areas(frame.area());
        render_input_bar(frame, app, input_area);
        render_world(frame, app, world_area);
        render_update_warning(frame, server_version, warn_area);
    } else {
        let [world_area, input_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(frame.area());
        render_input_bar(frame, app, input_area);
        render_world(frame, app, world_area);
    }
}

fn render_input_bar(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let input_text = Line::from(vec![
        Span::styled("> ", Style::new().fg(Color::Yellow).bold()),
        Span::raw(&app.input),
    ]);
    let widget = Paragraph::new(input_text)
        .block(Block::bordered().title("Say (Enter to send, Esc to quit, /map for map)"));
    frame.render_widget(widget, area);
}

fn render_world(frame: &mut Frame, app: &App, world_area: ratatui::layout::Rect) {
    let (page_ox, page_oy) = app.page_origin();

    let canvas_w = PAGE_TILES_W as f64;
    let canvas_h = PAGE_TILES_H as f64;

    let inner_h = (world_area.height.saturating_sub(2)) as f64;
    let inner_w = (world_area.width.saturating_sub(2)) as f64;
    let row_step = if inner_h > 1.0 {
        canvas_h / (inner_h - 1.0)
    } else {
        1.0
    };
    let col_step = if inner_w > 1.0 {
        canvas_w / (inner_w - 1.0)
    } else {
        1.0
    };

    let v_margin = row_step * 3.0;
    let clamp_y = |y: f64, dir: Direction| -> f64 {
        match dir {
            Direction::Up | Direction::Down => y.clamp(0.0, canvas_h - v_margin),
            _ => y,
        }
    };

    // Convert a world-tile position to canvas coordinates.
    // Canvas Y increases *upward*; tile rows increase *downward*, so we flip.
    let world_to_canvas = |wx: f32, wy: f32| -> (f64, f64) {
        let cx = (wx as f64) - (page_ox as f64);
        let cy = canvas_h - 1.0 - ((wy as f64) - (page_oy as f64));
        (cx, cy)
    };

    let (own_cx, own_cy_raw) = world_to_canvas(app.cursor.x, app.cursor.y);
    let own_cy = clamp_y(own_cy_raw, app.last_dir);
    let own_glyphs = boat_glyphs(&app.my_name, app.last_dir, row_step, col_step);

    let (ppx, ppy) = app.current_page();
    let update_note = match &app.update_status {
        UpdateStatus::Compatible {
            patch_available: Some(v),
        } => format!("  · v{v} available — brew upgrade black-sea"),
        UpdateStatus::Unknown => "  · server version unknown".to_string(),
        _ => String::new(),
    };
    let title = format!(
        "World  [{}, {}]  page ({}, {}){}",
        app.cursor.x as u32, app.cursor.y as u32, ppx, ppy, update_note
    );

    let canvas = Canvas::default()
        .block(Block::bordered().title(title))
        .x_bounds([0.0, canvas_w])
        .y_bounds([0.0, canvas_h])
        .paint(move |ctx: &mut Context| {
            draw_coastline(ctx, app, page_ox, page_oy, canvas_h);
            draw_remote_boats(
                ctx,
                app,
                &world_to_canvas,
                &clamp_y,
                row_step,
                col_step,
                canvas_w,
                canvas_h,
            );
            draw_own_boat(ctx, &own_glyphs, own_cx, own_cy);
            draw_offscreen_indicators(ctx, app, &world_to_canvas, canvas_w, canvas_h);
            draw_bubbles(ctx, app, &world_to_canvas);
        });

    frame.render_widget(canvas, world_area);
}

// ── Canvas drawing helpers ────────────────────────────────────────────────────

fn draw_coastline(ctx: &mut Context, app: &App, page_ox: u32, page_oy: u32, canvas_h: f64) {
    for tile_y in page_oy..page_oy + PAGE_TILES_H {
        for tile_x in page_ox..page_ox + PAGE_TILES_W {
            if let Some(Tile::Coast) = app.tile_at_world(tile_x, tile_y) {
                // Skip isolated coast tiles with no solid cardinal neighbour —
                // these are tiny islets that read as noise at this zoom level.
                let has_solid_neighbour = [
                    (tile_x.wrapping_sub(1), tile_y),
                    (tile_x + 1, tile_y),
                    (tile_x, tile_y.wrapping_sub(1)),
                    (tile_x, tile_y + 1),
                ]
                .iter()
                .any(|&(nx, ny)| matches!(app.tile_at_world(nx, ny), Some(t) if t != Tile::Water));
                if !has_solid_neighbour {
                    continue;
                }

                let cx = (tile_x - page_ox) as f64;
                let cy = canvas_h - 1.0 - (tile_y - page_oy) as f64;
                let ch = coast_char(app, tile_x, tile_y);
                ctx.print(cx, cy, Span::styled(ch, Style::new().fg(Color::Green)));
            }
        }
    }
}

fn draw_remote_boats(
    ctx: &mut Context,
    app: &App,
    world_to_canvas: &impl Fn(f32, f32) -> (f64, f64),
    clamp_y: &impl Fn(f64, Direction) -> f64,
    row_step: f64,
    col_step: f64,
    canvas_w: f64,
    canvas_h: f64,
) {
    for boat in app.remote_boats.values() {
        let (bx, by_raw) = world_to_canvas(boat.position.x, boat.position.y);
        if bx < 0.0 || bx > canvas_w || by_raw < 0.0 || by_raw > canvas_h {
            continue; // off-screen; 
        }
        let by = clamp_y(by_raw, boat.last_dir);
        for (x_off, y_off, glyph) in boat_glyphs(&boat.name, boat.last_dir, row_step, col_step) {
            ctx.print(
                bx + x_off,
                by + y_off,
                Span::styled(glyph, Style::new().fg(Color::Cyan)),
            );
        }
    }
}

fn draw_own_boat(ctx: &mut Context, own_glyphs: &[(f64, f64, String)], own_cx: f64, own_cy: f64) {
    for (x_off, y_off, glyph) in own_glyphs {
        ctx.print(
            own_cx + x_off,
            own_cy + y_off,
            Span::styled(
                glyph.clone(),
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
        );
    }
}

fn draw_offscreen_indicators(
    ctx: &mut Context,
    app: &App,
    world_to_canvas: &impl Fn(f32, f32) -> (f64, f64),
    canvas_w: f64,
    canvas_h: f64,
) {
    let center_x = canvas_w / 2.0;
    let center_y = canvas_h / 2.0;
    for boat in app.remote_boats.values() {
        let (bx, by_raw) = world_to_canvas(boat.position.x, boat.position.y);
        if bx >= 0.0 && bx <= canvas_w && by_raw >= 0.0 && by_raw <= canvas_h {
            continue; // already on screen
        }
        let dx = bx - center_x;
        let dy = by_raw - center_y;
        if dx == 0.0 && dy == 0.0 {
            continue;
        }
        // Find t so that center + t*(dx,dy) just touches the viewport border.
        let t = [
            if dx > 0.0 {
                (canvas_w - center_x) / dx
            } else if dx < 0.0 {
                -center_x / dx
            } else {
                f64::MAX
            },
            if dy > 0.0 {
                (canvas_h - center_y) / dy
            } else if dy < 0.0 {
                -center_y / dy
            } else {
                f64::MAX
            },
        ]
        .into_iter()
        .fold(f64::MAX, f64::min);
        let ix = (center_x + t * dx).clamp(0.0, canvas_w);
        let iy = (center_y + t * dy).clamp(0.0, canvas_h);
        ctx.print(ix, iy, Span::styled("*", Style::new().fg(Color::Cyan)));
    }
}

fn render_update_warning(frame: &mut Frame, server_version: &str, area: ratatui::layout::Rect) {
    let msg = Line::from(Span::styled(
        format!(
            "  Server is v{server_version} — your client is outdated.  Run: brew upgrade black-sea"
        ),
        Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(
        Paragraph::new(msg).block(Block::bordered().title("Client update required")),
        area,
    );
}

// ── Map overview ─────────────────────────────────────────────────────────────

fn render_map_overview(frame: &mut Frame, app: &App) {
    let overview = match &app.overview {
        Some(ov) => ov,
        None => return,
    };
    let ov_w = overview.width as f64;
    let ov_h = overview.height as f64;

    // Anchor positions are in full-map tile-space; scale to overview tile-space
    // using the protocol-level geometry constants.
    let world_w = MAP_TILES_W as f64;
    let world_h = MAP_TILES_H as f64;

    let selected_line = app
        .selected_anchor
        .and_then(|i| app.anchor_points.get(i))
        .map(|a| {
            let visited = if app.visited_anchors.contains(&a.id) {
                " [visited]"
            } else {
                ""
            };
            match a.note.as_deref() {
                Some(note) if !note.is_empty() => {
                    format!("  · {}{} — {}", a.name, visited, note)
                }
                _ => format!("  · {}{}", a.name, visited),
            }
        })
        .unwrap_or_default();

    let title = format!(
        "Map Overview  [{}, {}]  (Tab: cycle anchors · Enter: mark visited · Esc: close){}",
        app.cursor.x as u32, app.cursor.y as u32, selected_line,
    );

    let canvas = Canvas::default()
        .block(Block::bordered().title(title))
        .x_bounds([0.0, ov_w])
        .y_bounds([0.0, ov_h])
        .paint(|ctx: &mut Context| {
            draw_overview_terrain(ctx, overview);
            draw_overview_anchors(ctx, app, ov_w, ov_h, world_w, world_h);
            draw_overview_player(ctx, app, ov_w, ov_h, world_w, world_h);
        });

    frame.render_widget(canvas, frame.area());
}

fn draw_overview_terrain(ctx: &mut Context, overview: &crate::app::OverviewMap) {
    let w = overview.width;
    let h = overview.height;
    let h_f = h as f64;

    for row in 0..h {
        for col in 0..w {
            let idx = (row * w + col) as usize;
            let tile = overview.data[idx];
            let canvas_x = col as f64;
            let canvas_y = h_f - 1.0 - row as f64;
            match tile {
                Tile::Land => {
                    ctx.print(
                        canvas_x,
                        canvas_y,
                        Span::styled("#", Style::new().fg(Color::DarkGray)),
                    );
                }
                Tile::Coast => {
                    ctx.print(
                        canvas_x,
                        canvas_y,
                        Span::styled(".", Style::new().fg(Color::Green)),
                    );
                }
                Tile::Water => {}
            }
        }
    }
}

fn draw_overview_player(
    ctx: &mut Context,
    app: &App,
    ov_w: f64,
    ov_h: f64,
    world_w: f64,
    world_h: f64,
) {
    // Scale world position to overview coordinates.
    let ox = (app.cursor.x as f64 / world_w) * ov_w;
    let oy = (app.cursor.y as f64 / world_h) * ov_h;
    let canvas_y = ov_h - 1.0 - oy;
    ctx.print(
        ox,
        canvas_y,
        Span::styled(
            "X",
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
    );
}

fn draw_overview_anchors(
    ctx: &mut Context,
    app: &App,
    ov_w: f64,
    ov_h: f64,
    world_w: f64,
    world_h: f64,
) {
    for (i, anchor) in app.anchor_points.iter().enumerate() {
        let ox = (anchor.position.x as f64 / world_w) * ov_w;
        let oy = (anchor.position.y as f64 / world_h) * ov_h;
        let canvas_y = ov_h - 1.0 - oy;

        let visited = app.visited_anchors.contains(&anchor.id);
        let selected = app.selected_anchor == Some(i);
        let style = match (visited, selected) {
            (true, true) => Style::new()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::REVERSED),
            (true, false) => Style::new().fg(Color::DarkGray),
            (false, true) => Style::new()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            (false, false) => Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        };
        ctx.print(ox, canvas_y, Span::styled("\u{2693}", style));
    }
}

// ── Chat bubbles ─────────────────────────────────────────────────────────────

fn draw_bubbles(ctx: &mut Context, app: &App, world_to_canvas: &impl Fn(f32, f32) -> (f64, f64)) {
    for bubble in &app.bubbles {
        let (bx, by) = world_to_canvas(bubble.position.x, bubble.position.y);
        let age = bubble.received_at.elapsed().as_secs_f64();
        let color = if age < BUBBLE_TTL.as_secs_f64() * 0.7 {
            Color::White
        } else {
            Color::DarkGray
        };
        ctx.print(
            bx,
            by + BUBBLE_OFFSET as f64,
            Span::styled(format!("[ {} ]", bubble.text), Style::new().fg(color)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, WorldInfo};
    use black_sea_protocol::Tile;

    fn app_with_single_tile(col: usize, row: usize, tile: Tile) -> App {
        let chunk_size = 10u32;
        let mut grid = vec![vec![Tile::Water; 10]; 10];
        grid[row][col] = tile;

        let mut app = App::new("test".into());
        app.world_info = Some(WorldInfo {
            tile_width: 10,
            tile_height: 10,
            chunk_size,
        });

        let mut data: Vec<Tile> = Vec::with_capacity(100);
        for r in &grid {
            data.extend_from_slice(r);
        }
        app.loaded_chunks.insert((0, 0), data);
        app
    }

    /// Place a Coast tile at (5,5) and Land tiles at each (col_offset, row_offset) in `neighbors`.
    fn app_with_coast_and_neighbors(neighbors: &[(i32, i32)]) -> App {
        let chunk_size = 10u32;
        let mut grid = vec![vec![Tile::Water; 10]; 10];
        grid[5][5] = Tile::Coast;
        for &(dc, dr) in neighbors {
            grid[(5 + dr) as usize][(5 + dc) as usize] = Tile::Land;
        }

        let mut app = App::new("test".into());
        app.world_info = Some(WorldInfo {
            tile_width: 10,
            tile_height: 10,
            chunk_size,
        });

        let mut data: Vec<Tile> = Vec::with_capacity(100);
        for r in &grid {
            data.extend_from_slice(r);
        }
        app.loaded_chunks.insert((0, 0), data);
        app
    }

    #[test]
    fn coast_char_isolated_returns_space() {
        // A coast tile with no solid cardinal neighbours is suppressed (rendered as " ").
        // This protects the design rule: tiny isolated islets are hidden at this zoom level.
        let app = app_with_single_tile(5, 5, Tile::Coast);
        assert_eq!(coast_char(&app, 5, 5), " ");
    }

    #[test]
    fn coast_char_t_junction_ns_w() {
        // N+S+W neighbors → walking north-south → "|"
        let app = app_with_coast_and_neighbors(&[(0, -1), (0, 1), (-1, 0)]);
        assert_eq!(coast_char(&app, 5, 5), "|");
    }

    #[test]
    fn coast_char_t_junction_ns_e() {
        // N+S+E neighbors → walking north-south → "|"
        let app = app_with_coast_and_neighbors(&[(0, -1), (0, 1), (1, 0)]);
        assert_eq!(coast_char(&app, 5, 5), "|");
    }

    #[test]
    fn coast_char_t_junction_nwe() {
        // N+W+E neighbors → walking east-west → "-"
        let app = app_with_coast_and_neighbors(&[(0, -1), (-1, 0), (1, 0)]);
        assert_eq!(coast_char(&app, 5, 5), "-");
    }

    #[test]
    fn coast_char_t_junction_swe() {
        // S+W+E neighbors → walking east-west → "-"
        let app = app_with_coast_and_neighbors(&[(0, 1), (-1, 0), (1, 0)]);
        assert_eq!(coast_char(&app, 5, 5), "-");
    }

    #[test]
    fn coast_char_cross() {
        // All four cardinal neighbors → "+"
        let app = app_with_coast_and_neighbors(&[(0, -1), (0, 1), (-1, 0), (1, 0)]);
        assert_eq!(coast_char(&app, 5, 5), "+");
    }
}
