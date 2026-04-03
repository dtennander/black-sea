use anyhow::{Context, Result};

use crate::progress::make_download_bar;

const OSM_URL: &str = "https://osmdata.openstreetmap.de/download/land-polygons-complete-4326.zip";
const ZIP_NAME: &str = "land-polygons-complete-4326.zip";
const ETAG_NAME: &str = "land-polygons-complete-4326.zip.etag";

/// Top-level entry point: return the zip bytes, using the local disk cache when valid.
pub fn download_land_polygons() -> Result<Vec<u8>> {
    let cache = cache_dir();

    println!("[map] Checking remote ETag...");
    let remote_etag = fetch_remote_etag().unwrap_or_else(|e| {
        eprintln!("[map] Warning: could not fetch remote ETag ({e}), will re-download");
        None
    });

    if let Some(bytes) = load_from_cache(&cache, &remote_etag) {
        return Ok(bytes);
    }

    println!("[map] Downloading OSM land polygons...");
    let bytes = fetch_from_network()?;
    save_to_cache(&cache, &bytes, &remote_etag);
    Ok(bytes)
}

// ── Cache helpers ─────────────────────────────────────────────────────────────

/// Resolve the cache directory from `BLACK_SEA_CACHE_DIR` (default: `./osm-cache`).
fn cache_dir() -> std::path::PathBuf {
    let dir = std::env::var("BLACK_SEA_CACHE_DIR").unwrap_or_else(|_| "./osm-cache".to_string());
    let path = std::path::PathBuf::from(dir);
    if let Err(e) = std::fs::create_dir_all(&path) {
        eprintln!(
            "[map] Warning: could not create cache dir {}: {e}",
            path.display()
        );
    }
    path
}

/// Send a HEAD request and return the ETag header value, if present.
fn fetch_remote_etag() -> Result<Option<String>> {
    let resp = ureq::head(OSM_URL)
        .call()
        .context("failed to HEAD OSM URL")?;
    Ok(resp
        .headers()
        .get("etag")
        .map(|v| v.to_str().unwrap_or("").to_string()))
}

/// Try to load the cached zip from disk.
///
/// Returns `Some(bytes)` only if the zip file exists *and* its stored ETag matches `remote_etag`.
fn load_from_cache(cache_dir: &std::path::Path, remote_etag: &Option<String>) -> Option<Vec<u8>> {
    let zip_path = cache_dir.join(ZIP_NAME);
    let etag_path = cache_dir.join(ETAG_NAME);

    if !zip_path.exists() {
        println!("[map] No cached file found");
        return None;
    }

    if let Some(remote) = remote_etag {
        match std::fs::read_to_string(&etag_path) {
            Ok(stored) if stored.trim() == remote.trim() => {
                println!("[map] Cache valid (ETag matches), loading from disk...");
            }
            Ok(stored) => {
                println!(
                    "[map] Cache stale (ETag mismatch: stored={}, remote={})",
                    stored.trim(),
                    remote.trim()
                );
                return None;
            }
            Err(_) => {
                println!("[map] Cache ETag file missing or unreadable, re-downloading");
                return None;
            }
        }
    } else {
        println!("[map] Remote ETag unavailable, using cached file as-is");
    }

    match std::fs::read(&zip_path) {
        Ok(bytes) => {
            println!("[map] Loaded {} MiB from cache", bytes.len() / 1_048_576);
            Some(bytes)
        }
        Err(e) => {
            eprintln!("[map] Warning: failed to read cache file: {e}");
            None
        }
    }
}

/// Write the zip bytes and ETag sidecar to the cache directory.
/// Failures are logged as warnings but are non-fatal.
fn save_to_cache(cache_dir: &std::path::Path, bytes: &[u8], etag: &Option<String>) {
    let zip_path = cache_dir.join(ZIP_NAME);
    if let Err(e) = std::fs::write(&zip_path, bytes) {
        eprintln!(
            "[map] Warning: could not write cache file {}: {e}",
            zip_path.display()
        );
        return;
    }
    println!("[map] Saved {} MiB to cache", bytes.len() / 1_048_576);

    if let Some(tag) = etag {
        let etag_path = cache_dir.join(ETAG_NAME);
        if let Err(e) = std::fs::write(&etag_path, tag) {
            eprintln!(
                "[map] Warning: could not write ETag file {}: {e}",
                etag_path.display()
            );
        }
    }
}

/// Fetch the zip from the network, streaming it into memory with a progress bar.
fn fetch_from_network() -> Result<Vec<u8>> {
    use std::io::Read;

    const TWO_GIB: u64 = 2 * 1024 * 1024 * 1024;
    const CHUNK: usize = 65_536; // 64 KiB read chunks
    const LOG_INTERVAL_BYTES: u64 = 100 * 1024 * 1024; // log every 100 MiB in non-TTY mode

    let mut body = ureq::get(OSM_URL)
        .call()
        .context("failed to download OSM land polygon zip")?
        .into_body();

    let total = body.content_length();
    let mut reader = body.with_config().limit(TWO_GIB).reader();
    let mut bytes: Vec<u8> = match total {
        Some(n) => Vec::with_capacity(n as usize),
        None => Vec::new(),
    };

    let mut bar = make_download_bar(total, LOG_INTERVAL_BYTES);
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = reader.read(&mut buf).context("error reading download")?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        bar.set_position(bytes.len() as u64);
    }
    bar.finish();

    println!("[map] Downloaded {} MiB", bytes.len() / 1_048_576);
    Ok(bytes)
}
