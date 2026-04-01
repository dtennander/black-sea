use anyhow::Result;
use mmo_term::{recv_event, send_event, GameEvent};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

// Internal envelope carrying the sender's ID so each client can skip its own events.
#[derive(Clone)]
struct Envelope {
    sender_id: u64,
    event: GameEvent,
}

#[tokio::main]
async fn main() -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:7456").await?;
    let (tx, _) = broadcast::channel::<Envelope>(64);
    println!("Running Server");
    loop {
        let (socket, addr) = listener.accept().await?;
        println!("New connection from: {addr}");

        let tx = tx.clone();
        let rx = tx.subscribe();
        tokio::spawn(async move {
            if let Err(e) = handle(socket, tx, rx).await {
                eprintln!("Error handling connection from {addr}: {e}");
            }
        });
    }
}

async fn handle(
    mut stream: TcpStream,
    tx: broadcast::Sender<Envelope>,
    mut rx: broadcast::Receiver<Envelope>,
) -> Result<()> {
    let self_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

    loop {
        tokio::select! {
            // Incoming event from this client -> broadcast to all others
            result = recv_event(&mut stream) => {
                match result? {
                    Some(event) => {
                        match &event {
                            GameEvent::SayEvent { position, text } => {
                                println!("[SAY] ({}, {}): {}", position.x, position.y, text);
                            }
                        }
                        // Ignore send errors — it just means there are no other subscribers yet
                        let _ = tx.send(Envelope { sender_id: self_id, event });
                    }
                    None => {
                        println!("Client disconnected");
                        break;
                    }
                }
            }

            // Broadcast event from another client -> forward to this client
            result = rx.recv() => {
                match result {
                    Ok(envelope) if envelope.sender_id != self_id => {
                        send_event(&mut stream, &envelope.event).await?;
                    }
                    Ok(_) => {} // skip own events
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("Client {self_id} lagged, dropped {n} events");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    Ok(())
}
