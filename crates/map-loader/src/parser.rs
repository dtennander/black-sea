use anyhow::{Context, Result};
use geo::geometry::{Coord, LineString};
use shapefile::Shape;
use std::io::{Cursor, Read};

use crate::{BBOX_MAX_LAT, BBOX_MAX_LON, BBOX_MIN_LAT, BBOX_MIN_LON};

/// Parse OSM land polygons from a zip archive containing `.shp`, `.dbf`, and `.shx` files.
///
/// Only polygons whose bounding box overlaps the configured bounding box are returned.
pub fn parse_shapefile_from_zip(zip_bytes: &[u8]) -> Result<Vec<geo::geometry::Polygon<f64>>> {
    let cursor = Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).context("failed to open zip archive")?;

    let mut shp_buf: Option<Vec<u8>> = None;
    let mut dbf_buf: Option<Vec<u8>> = None;
    let mut shx_buf: Option<Vec<u8>> = None;

    for i in 0..archive.len() {
        let mut f = archive.by_index(i)?;
        let name = f.name().to_lowercase();
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        if name.ends_with(".shp") {
            shp_buf = Some(buf);
        } else if name.ends_with(".dbf") {
            dbf_buf = Some(buf);
        } else if name.ends_with(".shx") {
            shx_buf = Some(buf);
        }
    }

    let shp = shp_buf.context("no .shp file found in zip")?;
    let dbf = dbf_buf.context("no .dbf file found in zip")?;
    let shx = shx_buf.context("no .shx file found in zip")?;

    let shape_reader = shapefile::ShapeReader::with_shx(Cursor::new(shp), Cursor::new(shx))
        .context("failed to create ShapeReader")?;
    let dbase_reader =
        shapefile::dbase::Reader::new(Cursor::new(dbf)).context("failed to create dbase Reader")?;
    let mut reader = shapefile::Reader::new(shape_reader, dbase_reader);

    let mut polys: Vec<geo::geometry::Polygon<f64>> = Vec::new();

    for result in reader.iter_shapes_and_records() {
        let (shape, _) = result?;
        if let Shape::Polygon(poly) = shape {
            let bbox = poly.bbox();
            // Skip polygons entirely outside our bounding box.
            if bbox.max.x < BBOX_MIN_LON
                || bbox.min.x > BBOX_MAX_LON
                || bbox.max.y < BBOX_MIN_LAT
                || bbox.min.y > BBOX_MAX_LAT
            {
                continue;
            }
            for ring in poly.rings() {
                let coords: Vec<Coord<f64>> = ring
                    .points()
                    .iter()
                    .map(|p| Coord { x: p.x, y: p.y })
                    .collect();
                if coords.len() < 3 {
                    continue;
                }
                // TODO: handle interior rings (holes) so lakes / fjords inside
                // islands are correctly left as water.
                polys.push(geo::geometry::Polygon::new(LineString::new(coords), vec![]));
            }
        }
    }

    println!("[map] {} polygons after bbox clip", polys.len());
    Ok(polys)
}
