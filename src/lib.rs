use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GameEvent {
    /// A client said something at a position.
    SayEvent { position: Position, text: String },
    /// Sent by the server to a newly connected client to assign its ID and starting position.
    HelloEvent { your_id: u64, start_position: Position },
    /// Sent by the server immediately after HelloEvent with all currently connected players.
    WorldStateEvent { boats: Vec<(u64, Position)> },
    /// Broadcast whenever a client moves.
    MoveEvent { id: u64, position: Position },
    /// Broadcast by the server when a client disconnects.
    ByeEvent { id: u64 },
}

/// Serialize and send a `GameEvent` over `stream`.
///
/// Framing: 4-byte big-endian length prefix followed by the bincode payload.
pub async fn send_event(stream: &mut TcpStream, event: &GameEvent) -> Result<()> {
    let payload = bincode::serialize(event).context("failed to serialize event")?;
    let len = payload.len() as u32;
    stream
        .write_all(&len.to_be_bytes())
        .await
        .context("failed to write length prefix")?;
    stream
        .write_all(&payload)
        .await
        .context("failed to write event payload")?;
    Ok(())
}

/// Read and deserialize the next `GameEvent` from `stream`.
///
/// Returns `None` if the connection was closed cleanly (EOF on length header).
pub async fn recv_event(stream: &mut TcpStream) -> Result<Option<GameEvent>> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e).context("failed to read length prefix"),
    }
    let len = u32::from_be_bytes(len_buf) as usize;

    let mut payload = vec![0u8; len];
    stream
        .read_exact(&mut payload)
        .await
        .context("failed to read event payload")?;

    let event: GameEvent = bincode::deserialize(&payload).context("failed to deserialize event")?;
    Ok(Some(event))
}
