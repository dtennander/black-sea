use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use mmo_term::{recv_event, send_event, GameEvent, Position};
use rand::Rng;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

const GRID_SIZE: f32 = 100.0;
const MIN_SEPARATION: f32 = 5.0;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

// Internal envelope carrying the sender's ID so each client can skip its own events.
#[derive(Clone)]
struct Envelope {
    sender_id: u64,
    event: GameEvent,
}

type PositionMap = Arc<Mutex<HashMap<u64, Position>>>;

#[tokio::main]
async fn main() -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:7456").await?;
    let (tx, _) = broadcast::channel::<Envelope>(64);
    let positions: PositionMap = Arc::new(Mutex::new(HashMap::new()));
    println!("Running Server");
    loop {
        let (socket, addr) = listener.accept().await?;
        println!("New connection from: {addr}");

        let tx = tx.clone();
        let rx = tx.subscribe();
        let positions = Arc::clone(&positions);
        tokio::spawn(async move {
            if let Err(e) = handle(socket, tx, rx, positions).await {
                eprintln!("Error handling connection from {addr}: {e}");
            }
        });
    }
}

/// Pick a random position on the grid that is at least `MIN_SEPARATION` units
/// away from every currently occupied position.
fn find_free_position(positions: &HashMap<u64, Position>) -> Position {
    let mut rng = rand::rng();
    loop {
        let candidate = Position {
            x: rng.random_range(0.0..GRID_SIZE),
            y: rng.random_range(0.0..GRID_SIZE),
        };
        let too_close = positions.values().any(|p| {
            let dx = p.x - candidate.x;
            let dy = p.y - candidate.y;
            (dx * dx + dy * dy).sqrt() < MIN_SEPARATION
        });
        if !too_close {
            return candidate;
        }
    }
}

async fn handle(
    mut stream: TcpStream,
    tx: broadcast::Sender<Envelope>,
    mut rx: broadcast::Receiver<Envelope>,
    positions: PositionMap,
) -> Result<()> {
    let self_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

    // Assign a free starting position and tell the client.
    let start_position = {
        let mut map = positions.lock().unwrap();
        let pos = find_free_position(&map);
        map.insert(self_id, pos.clone());
        pos
    };
    send_event(
        &mut stream,
        &GameEvent::HelloEvent {
            your_id: self_id,
            start_position,
        },
    )
    .await?;

    // Send a snapshot of all other currently connected players.
    let snapshot: Vec<(u64, Position)> = positions
        .lock()
        .unwrap()
        .iter()
        .filter(|(id, _)| **id != self_id)
        .map(|(id, pos)| (*id, pos.clone()))
        .collect();
    send_event(&mut stream, &GameEvent::WorldStateEvent { boats: snapshot }).await?;

    loop {
        tokio::select! {
            // Incoming event from this client -> broadcast to all others
            result = recv_event(&mut stream) => {
                match result? {
                    Some(event) => {
                        let broadcast_event = match event {
                            GameEvent::SayEvent { ref position, ref text } => {
                                println!("[SAY] ({}, {}): {}", position.x, position.y, text);
                                event.clone()
                            }
                            // Overwrite the client-supplied id with the server-authoritative one.
                            GameEvent::MoveEvent { position, .. } => {
                                positions.lock().unwrap().insert(self_id, position.clone());
                                GameEvent::MoveEvent { id: self_id, position }
                            }
                            // Clients should not be sending these — ignore.
                            GameEvent::HelloEvent { .. }
                            | GameEvent::WorldStateEvent { .. }
                            | GameEvent::ByeEvent { .. } => continue,
                        };
                        let _ = tx.send(Envelope { sender_id: self_id, event: broadcast_event });
                    }
                    None => {
                        println!("Client {self_id} disconnected");
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

    // Clean up and notify remaining clients.
    positions.lock().unwrap().remove(&self_id);
    let _ = tx.send(Envelope {
        sender_id: self_id,
        event: GameEvent::ByeEvent { id: self_id },
    });

    Ok(())
}
