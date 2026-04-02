use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GameEvent {
    /// First message sent by a client after connecting, to register their chosen name.
    RegisterEvent { name: String },
    /// A client said something at a position.
    SayEvent { position: Option<Position>, text: String },
    /// Sent by the server to a newly connected client to assign its ID and starting position.
    HelloEvent { your_id: u64, start_position: Position },
    /// Sent by the server immediately after HelloEvent with all currently connected players (id, position, name).
    WorldStateEvent { boats: Vec<(u64, Position, String)> },
    /// Broadcast whenever a client moves.
    MoveEvent { id: u64, position: Position },
    /// Broadcast by the server when a new client joins, so existing clients learn their name.
    NameEvent { id: u64, name: String },
    /// Broadcast by the server when a client disconnects.
    ByeEvent { id: u64 },

    // ── Map protocol ──────────────────────────────────────────────────────────

    /// Sent by the server right after HelloEvent to describe the map dimensions and chunk layout.
    /// `tile_width` / `tile_height`: full map size in tiles.
    /// `chunk_size`: tiles per chunk (chunks are square).
    /// `meters_per_tile`: real-world scale, informational for now.
    WorldInfoEvent {
        tile_width: u32,
        tile_height: u32,
        chunk_size: u32,
        meters_per_tile: f32,
    },

    /// Sent by the client to request a single map chunk by its chunk-grid coordinates.
    MapChunkRequest { chunk_x: u32, chunk_y: u32 },

    /// Sent by the server in response to a MapChunkRequest.
    /// `data` is `chunk_size × chunk_size` bytes, row-major (row 0 = northernmost).
    /// Tile values: 0 = open water, 1 = coastline / grynnor (#), 2 = solid land.
    MapChunkResponse {
        chunk_x: u32,
        chunk_y: u32,
        data: Vec<u8>,
    },
}

/// Serialize and send a `GameEvent` over a WebSocket stream.
pub async fn send_event<S>(ws: &mut WebSocketStream<S>, event: &GameEvent) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let payload = bincode::serialize(event).context("failed to serialize event")?;
    ws.send(Message::Binary(payload.into()))
        .await
        .context("failed to send WebSocket message")?;
    Ok(())
}

/// Read and deserialize the next `GameEvent` from a WebSocket stream.
///
/// Returns `None` if the connection was closed cleanly.
pub async fn recv_event<S>(ws: &mut WebSocketStream<S>) -> Result<Option<GameEvent>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        match ws.next().await {
            None => return Ok(None),
            Some(Err(e)) => return Err(e).context("WebSocket receive error"),
            Some(Ok(Message::Binary(payload))) => {
                let event: GameEvent =
                    bincode::deserialize(&payload).context("failed to deserialize event")?;
                return Ok(Some(event));
            }
            Some(Ok(Message::Close(_))) => return Ok(None),
            Some(Ok(_)) => continue, // skip Ping, Pong, Text
        }
    }
}
