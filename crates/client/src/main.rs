mod app;
mod input;
mod render;

use anyhow::Result;
use black_sea_protocol::{GameEvent, recv_event, send_event};
use semver::Version;
use tokio::time::{Duration, timeout};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use input::prompt_name;

type ClientWs = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

const VERSION_TIMEOUT_SECS: u64 = 5;

enum CompatResult {
    Compatible,
    Incompatible { server_version: String },
}

async fn check_server_version(ws: &mut ClientWs) -> Result<CompatResult> {
    let own = Version::parse(env!("GIT_VERSION")).unwrap_or(Version::new(0, 0, 0));
    timeout(Duration::from_secs(VERSION_TIMEOUT_SECS), async {
        loop {
            match recv_event(ws).await? {
                Some(GameEvent::ServerVersionEvent { version }) => {
                    return Ok(match Version::parse(&version) {
                        Ok(srv) if srv.major == own.major && srv.minor == own.minor => {
                            CompatResult::Compatible
                        }
                        _ => CompatResult::Incompatible { server_version: version },
                    });
                }
                Some(_) => continue,
                None => {
                    return Err(anyhow::anyhow!("Server disconnected before version exchange"))
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| Err(anyhow::anyhow!("Timed out waiting for server version")))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("black-sea {}", env!("GIT_VERSION"));
        return Ok(());
    }

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let server_url = std::env::var("BLACK_SEA_SERVER").unwrap_or_else(|_| {
        option_env!("BLACK_SEA_SERVER_DEFAULT")
            .unwrap_or("ws://127.0.0.1:7456")
            .to_string()
    });

    let mut terminal = ratatui::init();

    // Phase 1: Connect
    let request = server_url.into_client_request()?;
    let (mut ws, _) = match connect_async(request).await {
        Ok(conn) => conn,
        Err(e) => {
            ratatui::restore();
            return Err(e.into());
        }
    };

    // Phase 2: Check version before showing the name prompt
    let compat = match check_server_version(&mut ws).await {
        Ok(c) => c,
        Err(e) => {
            ratatui::restore();
            return Err(e);
        }
    };

    if let CompatResult::Incompatible { server_version } = compat {
        let result = input::show_incompatible_screen(&mut terminal, &server_version).await;
        ratatui::restore();
        return result;
    }

    // Phase 3: Name prompt → register → game
    let name = match prompt_name(&mut terminal).await {
        Ok(n) => n,
        Err(e) => {
            ratatui::restore();
            return Err(e);
        }
    };

    send_event(&mut ws, &GameEvent::RegisterEvent { name: name.clone() }).await?;

    let result = app::run(&mut terminal, &mut ws, name).await;
    ratatui::restore();
    result
}
