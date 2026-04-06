use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use black_sea_protocol::{GameEvent, MapGrid, Position, recv_event, send_event};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};

use crate::metrics;
use crate::spawn::find_free_position;

// ── Shared state types ────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct BoatEntry {
    pub position: Position,
    pub name: String,
}

pub type BoatMap = Arc<Mutex<HashMap<u64, BoatEntry>>>;

/// A broadcast envelope wrapping a [`GameEvent`] with its sender's ID so we
/// can avoid echoing events back to the originating client.
#[derive(Clone)]
pub struct Envelope {
    pub sender_id: u64,
    pub event: GameEvent,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

// ── Per-client handler ────────────────────────────────────────────────────────

/// Drive a single connected client for its entire lifetime.
///
/// Responsibilities:
/// - Wait for the initial `RegisterEvent`.
/// - Find a free spawn position; close the connection if the world is full.
/// - Send `HelloEvent`, `WorldInfoEvent`, and a `WorldStateEvent` snapshot.
/// - Enter the main event loop: forward moves/chat to the broadcast channel and
///   relay broadcast events from other clients to this client's WebSocket.
/// - On disconnect: remove from the boat map and broadcast `ByeEvent`.
pub async fn handle(
    mut ws: WebSocketStream<TcpStream>,
    tx: broadcast::Sender<Envelope>,
    mut rx: broadcast::Receiver<Envelope>,
    boats: BoatMap,
    map: Arc<MapGrid>,
    metres_per_tile: f32,
) -> Result<()> {
    let self_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

    // Send version immediately so the client can check compatibility before registering.
    send_event(
        &mut ws,
        &GameEvent::ServerVersionEvent {
            version: env!("CLIENT_VERSION").to_string(),
        },
    )
    .await?;

    // ── Registration ──────────────────────────────────────────────────────────
    let name = loop {
        match recv_event(&mut ws).await? {
            Some(GameEvent::RegisterEvent { name }) => break name,
            Some(_) => {}
            None => return Ok(()),
        }
    };

    // ── Spawn ─────────────────────────────────────────────────────────────────
    let spawn_result = {
        let mut m = boats.lock().unwrap();
        match find_free_position(&m, &map) {
            Some(pos) => {
                m.insert(
                    self_id,
                    BoatEntry {
                        position: pos.clone(),
                        name: name.clone(),
                    },
                );
                Some(pos)
            }
            None => None,
        }
    }; // MutexGuard dropped here, before any .await

    let start_position = match spawn_result {
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
    };

    // ── Handshake ─────────────────────────────────────────────────────────────
    send_event(
        &mut ws,
        &GameEvent::HelloEvent {
            your_id: self_id,
            start_position,
        },
    )
    .await?;

    send_event(
        &mut ws,
        &GameEvent::WorldInfoEvent {
            tile_width: map.width,
            tile_height: map.height,
            chunk_size: map.chunk_size,
            meters_per_tile: metres_per_tile,
        },
    )
    .await?;

    let snapshot: Vec<(u64, Position, String)> = boats
        .lock()
        .unwrap()
        .iter()
        .filter(|(id, _)| **id != self_id)
        .map(|(id, e)| (*id, e.position.clone(), e.name.clone()))
        .collect();
    send_event(&mut ws, &GameEvent::WorldStateEvent { boats: snapshot }).await?;

    let _ = tx.send(Envelope {
        sender_id: self_id,
        event: GameEvent::NameEvent {
            id: self_id,
            name: name.clone(),
        },
    });

    println!("Client {self_id} registered as '{name}'");

    // ── Main event loop ───────────────────────────────────────────────────────
    let loop_result: Result<()> = async {
        loop {
            tokio::select! {
                result = recv_event(&mut ws) => {
                    match result? {
                        Some(event) => {
                            let broadcast_event = match event {
                                GameEvent::SayEvent { position: client_pos, ref text } => {
                                    metrics::CHAT_MESSAGES_TOTAL.inc();
                                    let authoritative_pos = {
                                        let m = boats.lock().unwrap();
                                        m.get(&self_id).map(|e| e.position.clone())
                                    }
                                    .or(client_pos);
                                    match &authoritative_pos {
                                        Some(p) => println!("[SAY] ({}, {}): {}", p.x, p.y, text),
                                        None    => println!("[SAY] (unknown): {}", text),
                                    }
                                    GameEvent::SayEvent {
                                        position: authoritative_pos,
                                        text: text.clone(),
                                    }
                                }

                                GameEvent::MoveEvent { position, .. } => {
                                    if !map.is_passable(&position) {
                                        metrics::INVALID_MOVES_TOTAL.inc();
                                        continue; // silently drop invalid moves
                                    }
                                    let mut m = boats.lock().unwrap();
                                    if let Some(entry) = m.get_mut(&self_id) {
                                        let dx = (entry.position.x - position.x) as f64;
                                        let dy = (entry.position.y - position.y) as f64;
                                        let tiles = (dx * dx + dy * dy).sqrt();
                                        // Each tile is 20m; 1 nautical mile = 1852m
                                        let nm = tiles * 20.0 / 1852.0;
                                        metrics::NAUTICAL_MILES_SAILED.inc_by((nm * 1000.0) as u64);
                                        entry.position = position.clone();
                                    }
                                    metrics::MOVES_TOTAL.inc();
                                    GameEvent::MoveEvent { id: self_id, position }
                                }

                                GameEvent::MapChunkRequest { chunk_x, chunk_y } => {
                                    metrics::MAP_CHUNK_REQUESTS_TOTAL.inc();
                                    let data = map.chunk_data(chunk_x, chunk_y);
                                    send_event(
                                        &mut ws,
                                        &GameEvent::MapChunkResponse { chunk_x, chunk_y, data },
                                    )
                                    .await?;
                                    continue; // response already sent; nothing to broadcast
                                }

                                // These are never valid incoming messages from a client.
                                GameEvent::RegisterEvent { .. }
                                | GameEvent::HelloEvent { .. }
                                | GameEvent::ServerVersionEvent { .. }
                                | GameEvent::WorldStateEvent { .. }
                                | GameEvent::WorldInfoEvent { .. }
                                | GameEvent::NameEvent { .. }
                                | GameEvent::ByeEvent { .. }
                                | GameEvent::MapChunkResponse { .. } => continue,
                            };

                            let _ = tx.send(Envelope {
                                sender_id: self_id,
                                event: broadcast_event,
                            });
                        }
                        None => {
                            println!("Client {self_id} disconnected");
                            break;
                        }
                    }
                }

                result = rx.recv() => {
                    match result {
                        Ok(envelope) if envelope.sender_id != self_id => {
                            send_event(&mut ws, &envelope.event).await?;
                        }
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            eprintln!("Client {self_id} lagged, dropped {n} events");
                            metrics::BROADCAST_LAG_TOTAL.inc_by(n);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    // ── Cleanup ───────────────────────────────────────────────────────────────
    boats.lock().unwrap().remove(&self_id);
    metrics::ACTIVE_CONNECTIONS.dec();
    let _ = tx.send(Envelope {
        sender_id: self_id,
        event: GameEvent::ByeEvent { id: self_id },
    });

    loop_result
}
