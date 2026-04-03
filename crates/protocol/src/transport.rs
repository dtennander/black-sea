use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use crate::GameEvent;

/// Serialize and send a [`GameEvent`] over a WebSocket stream.
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

/// Read and deserialize the next [`GameEvent`] from a WebSocket stream.
///
/// Returns `None` if the connection was closed cleanly.
/// Ping, Pong, and Text frames are silently skipped.
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
