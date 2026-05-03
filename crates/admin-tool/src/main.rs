use std::{path::PathBuf, sync::Arc};

use clap::Parser;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "anchorings.csv")]
    csv: PathBuf,
    #[arg(long, default_value_t = 3000)]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let stats = black_sea_admin_tool::Stats {
        active_connections: Arc::new(|| 0),
        total_connections: Arc::new(|| 0),
    };
    black_sea_admin_tool::serve(&format!("0.0.0.0:{}", args.port), args.csv, stats, "").await
}
