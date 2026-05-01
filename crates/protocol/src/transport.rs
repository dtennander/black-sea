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
                match bincode::deserialize(&payload) {
                    Ok(event) => return Ok(Some(event)),
                    Err(_) => continue, // unknown/future variant — skip silently
                }
            }
            Some(Ok(Message::Close(_))) => return Ok(None),
            Some(Ok(_)) => continue, // skip Ping, Pong, Text
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    async fn loopback() -> (
        WebSocketStream<tokio::io::DuplexStream>,
        WebSocketStream<tokio::io::DuplexStream>,
    ) {
        let (a, b) = tokio::io::duplex(65536);
        let accept = tokio_tungstenite::accept_async(b);
        let connect = tokio_tungstenite::client_async("ws://unused/", a);
        // Run both handshakes concurrently
        let (client_result, server_result) = tokio::join!(connect, accept);
        let (client_ws, _) = client_result.unwrap();
        let server_ws = server_result.unwrap();
        (client_ws, server_ws)
    }

    #[tokio::test]
    async fn recv_event_known_variant_returns_event() {
        let (mut sender, mut receiver) = loopback().await;
        let event = GameEvent::SayEvent {
            position: None,
            text: "hi".into(),
        };
        let bytes = bincode::serialize(&event).unwrap();
        sender.send(Message::Binary(bytes.into())).await.unwrap();
        let result = recv_event(&mut receiver).await.unwrap();
        assert!(matches!(result, Some(GameEvent::SayEvent { .. })));
    }

    #[tokio::test]
    async fn recv_event_unknown_binary_frame_skips_and_returns_next() {
        let (mut sender, mut receiver) = loopback().await;
        // Send garbage bytes that won't deserialize as any GameEvent
        sender
            .send(Message::Binary(vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF].into()))
            .await
            .unwrap();
        // Then send a valid event
        let event = GameEvent::SayEvent {
            position: None,
            text: "after".into(),
        };
        let bytes = bincode::serialize(&event).unwrap();
        sender.send(Message::Binary(bytes.into())).await.unwrap();
        let result = recv_event(&mut receiver).await.unwrap();
        assert!(matches!(result, Some(GameEvent::SayEvent { .. })));
    }

    #[tokio::test]
    async fn recv_event_close_returns_none() {
        let (mut sender, mut receiver) = loopback().await;
        sender.close(None).await.unwrap();
        let result = recv_event(&mut receiver).await.unwrap();
        assert!(result.is_none());
    }
}
