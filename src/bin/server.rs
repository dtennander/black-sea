use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use black_sea::{GameEvent, Position, recv_event, send_event};
use rand::Rng;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};

const GRID_SIZE: f32 = 100.0;
const MIN_SEPARATION: f32 = 5.0;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

// Internal envelope carrying the sender's ID so each client can skip its own events.
#[derive(Clone)]
struct Envelope {
    sender_id: u64,
    event: GameEvent,
}

/// World state entry: position + player name.
#[derive(Clone)]
struct BoatEntry {
    position: Position,
    name: String,
}

type BoatMap = Arc<Mutex<HashMap<u64, BoatEntry>>>;

#[tokio::main]
async fn main() -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:7456").await?;
    let (tx, _) = broadcast::channel::<Envelope>(64);
    let boats: BoatMap = Arc::new(Mutex::new(HashMap::new()));
    println!("Running Server");
    loop {
        let (socket, addr) = listener.accept().await?;
        println!("New connection from: {addr}");

        let tx = tx.clone();
        let rx = tx.subscribe();
        let boats = Arc::clone(&boats);
        tokio::spawn(async move {
            match accept_async(socket).await {
                Ok(ws) => {
                    if let Err(e) = handle(ws, tx, rx, boats).await {
                        eprintln!("Error handling connection from {addr}: {e}");
                    }
                }
                Err(e) => eprintln!("WebSocket handshake failed from {addr}: {e}"),
            }
        });
    }
}

const MAX_PLACEMENT_ATTEMPTS: usize = 1000;

/// Pick a random position on the grid that is at least `MIN_SEPARATION` units
/// away from every currently occupied position.
/// Returns `None` if no free position could be found after `MAX_PLACEMENT_ATTEMPTS` tries.
fn find_free_position(boats: &HashMap<u64, BoatEntry>) -> Option<Position> {
    let mut rng = rand::rng();
    for _ in 0..MAX_PLACEMENT_ATTEMPTS {
        let candidate = Position {
            x: rng.random_range(0.0..GRID_SIZE),
            y: rng.random_range(0.0..GRID_SIZE),
        };
        let too_close = boats.values().any(|entry| {
            let dx = entry.position.x - candidate.x;
            let dy = entry.position.y - candidate.y;
            (dx * dx + dy * dy).sqrt() < MIN_SEPARATION
        });
        if !too_close {
            return Some(candidate);
        }
    }
    None
}

async fn handle(
    mut ws: WebSocketStream<TcpStream>,
    tx: broadcast::Sender<Envelope>,
    mut rx: broadcast::Receiver<Envelope>,
    boats: BoatMap,
) -> Result<()> {
    let self_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

    // Wait for the client to send a RegisterEvent with their chosen name.
    let name = loop {
        match recv_event(&mut ws).await? {
            Some(GameEvent::RegisterEvent { name }) => break name,
            Some(_) => {} // ignore anything else until we get a name
            None => return Ok(()), // client disconnected before registering
        }
    };

    // Assign a free starting position and tell the client.
    let start_position = {
        let pos = {
            let mut map = boats.lock().unwrap();
            match find_free_position(&map) {
                Some(pos) => {
                    map.insert(self_id, BoatEntry { position: pos.clone(), name: name.clone() });
                    Some(pos)
                }
                None => None,
            }
        }; // MutexGuard dropped here
        match pos {
            Some(pos) => pos,
            None => {
                let _ = ws
                    .close(Some(CloseFrame {
                        code: CloseCode::Again,
                        reason: "World is full".into(),
                    }))
                    .await;
                return Ok(());
            }
        }
    };

    send_event(
        &mut ws,
        &GameEvent::HelloEvent {
            your_id: self_id,
            start_position,
        },
    )
    .await?;

    // Send a snapshot of all other currently connected players (id, position, name).
    let snapshot: Vec<(u64, Position, String)> = boats
        .lock()
        .unwrap()
        .iter()
        .filter(|(id, _)| **id != self_id)
        .map(|(id, entry)| (*id, entry.position.clone(), entry.name.clone()))
        .collect();
    send_event(&mut ws, &GameEvent::WorldStateEvent { boats: snapshot }).await?;

    // Broadcast to all existing clients that a new named boat has arrived.
    let _ = tx.send(Envelope {
        sender_id: self_id,
        event: GameEvent::NameEvent { id: self_id, name: name.clone() },
    });

    println!("Client {self_id} registered as '{name}'");

    let loop_result: Result<()> = async {
        loop {
            tokio::select! {
                // Incoming event from this client -> broadcast to all others
                result = recv_event(&mut ws) => {
                    match result? {
                        Some(event) => {
                            let broadcast_event = match event {
                                GameEvent::SayEvent { position: client_pos, ref text } => {
                                    let authoritative_pos = boats
                                        .lock()
                                        .unwrap()
                                        .get(&self_id)
                                        .map(|e| e.position.clone())
                                        .or(client_pos);
                                    match &authoritative_pos {
                                        Some(p) => println!("[SAY] ({}, {}): {}", p.x, p.y, text),
                                        None => println!("[SAY] (unknown): {}", text),
                                    }
                                    GameEvent::SayEvent { position: authoritative_pos, text: text.clone() }
                                }
                                // Overwrite the client-supplied id with the server-authoritative one.
                                GameEvent::MoveEvent { position, .. } => {
                                    let mut map = boats.lock().unwrap();
                                    if let Some(entry) = map.get_mut(&self_id) {
                                        entry.position = position.clone();
                                    }
                                    GameEvent::MoveEvent { id: self_id, position }
                                }
                                // Clients should not be sending these — ignore.
                                GameEvent::RegisterEvent { .. }
                                | GameEvent::HelloEvent { .. }
                                | GameEvent::WorldStateEvent { .. }
                                | GameEvent::NameEvent { .. }
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
                            send_event(&mut ws, &envelope.event).await?;
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
    }.await;

    // Clean up and notify remaining clients — always runs, even on error.
    boats.lock().unwrap().remove(&self_id);
    let _ = tx.send(Envelope {
        sender_id: self_id,
        event: GameEvent::ByeEvent { id: self_id },
    });

    loop_result
}
