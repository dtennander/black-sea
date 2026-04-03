mod app;
mod input;
mod render;

use anyhow::Result;
use black_sea_protocol::{GameEvent, send_event};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use input::prompt_name;

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let server_url = std::env::var("BLACK_SEA_SERVER").unwrap_or_else(|_| {
        option_env!("BLACK_SEA_SERVER_DEFAULT")
            .unwrap_or("ws://127.0.0.1:7456")
            .to_string()
    });

    let mut terminal = ratatui::init();

    let name = match prompt_name(&mut terminal).await {
        Ok(n) => n,
        Err(e) => {
            ratatui::restore();
            return Err(e);
        }
    };

    let request = server_url.into_client_request()?;
    let (mut ws, _) = connect_async(request).await?;

    send_event(&mut ws, &GameEvent::RegisterEvent { name: name.clone() }).await?;

    let result = app::run(&mut terminal, &mut ws, name).await;
    ratatui::restore();
    result
}
