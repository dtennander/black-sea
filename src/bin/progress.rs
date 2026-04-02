/// Map-loading progress reporting.
///
/// Abstracts over two output modes:
/// - **TTY**: a rich [`indicatif`] progress bar rendered in-place.
/// - **Non-TTY** (e.g. Docker / CI logs): periodic `println!` messages at a
///   configurable interval so log files stay readable without the noise of a
///   rewriting progress bar.
///
/// Two factory functions cover all current use-cases:
/// - [`make_count_bar`] — `{pos}/{len}` style, used for polygon simplification
///   and rasterization.
/// - [`make_download_bar`] — byte-transfer style with an optional spinner when
///   the total size is unknown, used for the OSM zip download.

#[cfg(feature = "server-map")]
mod inner {
    use indicatif::{ProgressBar, ProgressStyle};
    use std::io::IsTerminal;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    fn is_tty() -> bool {
        std::io::stdout().is_terminal()
    }

    // ── Count bar (pos / len) ─────────────────────────────────────────────────

    /// State for non-TTY count-style progress (shared across threads via Arc).
    struct CountFallback {
        total: u64,
        label: String,
        log_interval: u64,
        /// Tracks how many items have been reported so far (atomic for par_iter).
        counter: AtomicU64,
    }

    enum CountInner {
        Bar(ProgressBar),
        Fallback(Arc<CountFallback>),
    }

    /// Progress tracker for counted work (polygon simplification, rasterization).
    ///
    /// Cheap to clone — both variants are internally reference-counted.
    pub struct CountProgress(Arc<CountInner>);

    impl Clone for CountProgress {
        fn clone(&self) -> Self {
            CountProgress(Arc::clone(&self.0))
        }
    }

    impl CountProgress {
        /// Advance the counter by one step.
        pub fn inc(&self) {
            match self.0.as_ref() {
                CountInner::Bar(b) => b.inc(1),
                CountInner::Fallback(f) => {
                    let done = f.counter.fetch_add(1, Ordering::Relaxed) + 1;
                    if done % f.log_interval == 0 || done == f.total {
                        println!(
                            "[map] {}... {}/{} {} ({}%)",
                            capitalize(&f.label),
                            done,
                            f.total,
                            f.label,
                            done * 100 / f.total
                        );
                    }
                }
            }
        }

        /// Mark the operation as finished and clean up the bar (if any).
        pub fn finish(self) {
            // Unwrap the Arc — this is called once, after all work is done.
            match Arc::try_unwrap(self.0) {
                Ok(inner) => {
                    if let CountInner::Bar(b) = inner {
                        b.finish_and_clear();
                    }
                }
                Err(arc) => {
                    // Shouldn't happen in practice, but be safe.
                    if let CountInner::Bar(b) = arc.as_ref() {
                        b.finish_and_clear();
                    }
                }
            }
        }
    }

    /// Create a `{pos}/{len}` style progress tracker.
    ///
    /// - `len`          — total number of items.
    /// - `item_label`   — noun used in both the bar template and the fallback
    ///                    `println!` messages (e.g. `"polygons simplified"`).
    /// - `log_interval` — how often to emit a log line in non-TTY mode.
    pub fn make_count_bar(len: u64, item_label: &str, log_interval: u64) -> CountProgress {
        let inner = if is_tty() {
            let bar = ProgressBar::new(len);
            bar.set_style(
                ProgressStyle::with_template(&format!(
                    "[map] [{{bar:50.yellow/white}}] {{pos}}/{{len}} {item_label} ({{percent}}%, eta {{eta}})"
                ))
                .unwrap()
                .progress_chars("=>-"),
            );
            CountInner::Bar(bar)
        } else {
            CountInner::Fallback(Arc::new(CountFallback {
                total: len,
                label: item_label.to_string(),
                log_interval,
                counter: AtomicU64::new(0),
            }))
        };
        CountProgress(Arc::new(inner))
    }

    // ── Download bar (bytes) ──────────────────────────────────────────────────

    enum DownloadInner {
        Bar(ProgressBar),
        Fallback {
            total: Option<u64>,
            log_interval_bytes: u64,
            logged_threshold: u64,
        },
    }

    /// Progress tracker for a streaming byte download.
    pub struct DownloadProgress(DownloadInner);

    impl DownloadProgress {
        /// Update the progress to `bytes_received` bytes downloaded so far.
        ///
        /// In non-TTY mode this emits a log line each time the downloaded
        /// amount crosses the next `log_interval_bytes` boundary.
        pub fn set_position(&mut self, bytes_received: u64) {
            match &mut self.0 {
                DownloadInner::Bar(b) => b.set_position(bytes_received),
                DownloadInner::Fallback {
                    total,
                    log_interval_bytes,
                    logged_threshold,
                } => {
                    if bytes_received >= *logged_threshold + *log_interval_bytes {
                        *logged_threshold =
                            (bytes_received / *log_interval_bytes) * *log_interval_bytes;
                        match total {
                            Some(t) => println!(
                                "[map] Downloading... {} MiB / {} MiB ({}%)",
                                bytes_received / 1_048_576,
                                *t / 1_048_576,
                                bytes_received * 100 / *t,
                            ),
                            None => {
                                println!("[map] Downloading... {} MiB", bytes_received / 1_048_576)
                            }
                        }
                    }
                }
            }
        }

        /// Mark the download as finished and clean up the bar (if any).
        pub fn finish(self) {
            if let DownloadInner::Bar(b) = self.0 {
                b.finish_and_clear();
            }
        }
    }

    /// Create a download progress tracker.
    ///
    /// - `total`              — total expected bytes, or `None` if unknown
    ///                          (renders as a spinner in TTY mode).
    /// - `log_interval_bytes` — byte interval between log lines in non-TTY mode.
    pub fn make_download_bar(total: Option<u64>, log_interval_bytes: u64) -> DownloadProgress {
        let inner = if is_tty() {
            let bar = match total {
                Some(n) => {
                    let b = ProgressBar::new(n);
                    b.set_style(
                        ProgressStyle::with_template(
                            "[map] [{bar:50.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, eta {eta})",
                        )
                        .unwrap()
                        .progress_chars("=>-"),
                    );
                    b
                }
                None => {
                    let b = ProgressBar::new_spinner();
                    b.set_style(
                        ProgressStyle::with_template(
                            "[map] {spinner:.cyan} {bytes} downloaded ({bytes_per_sec})",
                        )
                        .unwrap(),
                    );
                    b.enable_steady_tick(Duration::from_millis(120));
                    b
                }
            };
            DownloadInner::Bar(bar)
        } else {
            DownloadInner::Fallback {
                total,
                log_interval_bytes,
                logged_threshold: 0,
            }
        };
        DownloadProgress(inner)
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn capitalize(s: &str) -> String {
        let mut c = s.chars();
        match c.next() {
            None => String::new(),
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        }
    }
}

#[cfg(feature = "server-map")]
pub use inner::{make_count_bar, make_download_bar};
