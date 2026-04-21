use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use black_sea_protocol::{AnchorPoint, Position};
use serde::Deserialize;

/// Raw CSV row — matches the seed `anchorings.csv` header `name,x,y,note`.
#[derive(Debug, Deserialize)]
struct Row {
    name: String,
    x: f32,
    y: f32,
    note: Option<String>,
}

/// Load anchor points from a CSV file on disk.
///
/// Missing files are not an error: a warning is logged and an empty `Vec` is
/// returned so the server can still start in dev environments without the seed
/// data present.
pub fn load_anchorings(path: &Path) -> Result<Vec<AnchorPoint>> {
    if !path.exists() {
        println!(
            "[anchorings] {} not found — starting with no anchor points",
            path.display()
        );
        return Ok(Vec::new());
    }

    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let points = read_anchorings(file)
        .with_context(|| format!("parsing anchorings from {}", path.display()))?;
    println!(
        "[anchorings] Loaded {} anchor point(s) from {}",
        points.len(),
        path.display()
    );
    Ok(points)
}

/// Parse anchor points from any [`Read`] source. `id` is assigned from the row
/// index so ordering in the CSV is stable within a single server run.
fn read_anchorings<R: Read>(reader: R) -> Result<Vec<AnchorPoint>> {
    let mut rdr = csv::Reader::from_reader(reader);
    let mut out = Vec::new();
    for (idx, record) in rdr.deserialize::<Row>().enumerate() {
        let row = record.with_context(|| format!("row {}", idx + 1))?;
        // Empty `note` cells deserialize as `Some("")`; normalise to `None`
        // so clients can treat "no note" uniformly.
        let note = row.note.filter(|s| !s.is_empty());
        out.push(AnchorPoint {
            id: idx as u32,
            name: row.name,
            position: Position { x: row.x, y: row.y },
            note,
        });
    }
    Ok(out)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rows_and_assigns_sequential_ids() {
        let csv = "\
name,x,y,note
Sandhamn,4794.0,2812.1,Classic starting point
Möja,4793.0,2371.1,
Finnhamn,4674.0,1899.1,Outer archipelago
";
        let points = read_anchorings(csv.as_bytes()).expect("parses");
        assert_eq!(points.len(), 3);

        assert_eq!(points[0].id, 0);
        assert_eq!(points[0].name, "Sandhamn");
        assert_eq!(points[0].position.x, 4794.0);
        assert_eq!(points[0].position.y, 2812.1);
        assert_eq!(points[0].note.as_deref(), Some("Classic starting point"));

        // Empty `note` cell should become `None`, not `Some("")`.
        assert_eq!(points[1].id, 1);
        assert_eq!(points[1].name, "Möja");
        assert!(points[1].note.is_none());

        assert_eq!(points[2].id, 2);
        assert_eq!(points[2].note.as_deref(), Some("Outer archipelago"));
    }

    #[test]
    fn empty_csv_yields_no_points() {
        let csv = "name,x,y,note\n";
        let points = read_anchorings(csv.as_bytes()).expect("parses");
        assert!(points.is_empty());
    }
}
