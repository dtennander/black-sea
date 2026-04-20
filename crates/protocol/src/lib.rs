pub mod coords;
pub mod tiles;
pub mod transport;

pub use tiles::{MapGrid, Tile};
pub use transport::{recv_event, send_event};

use serde::{Deserialize, Serialize};

/// A position in tile-space coordinates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

/// A named location in tile-space — typically a favourite anchoring spot.
///
/// Loaded by the server from a CSV at startup and broadcast to clients in
/// [`GameEvent::AnchorPointsEvent`] during the handshake. `id` is assigned by
/// the server (stable for a given CSV row order within a run).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorPoint {
    pub id: u32,
    pub name: String,
    pub position: Position,
    pub note: Option<String>,
}

/// The full wire protocol between client and server.
///
/// # Direction conventions
///
/// | Variant | Direction |
/// |---|---|
/// | `RegisterEvent` | Client → Server |
/// | `HelloEvent` | Server → Client |
/// | `ServerVersionEvent` | Server → Client |
/// | `WorldInfoEvent` | Server → Client |
/// | `WorldStateEvent` | Server → Client |
/// | `NameEvent` | Server → Client |
/// | `ByeEvent` | Server → Client |
/// | `MapChunkRequest` | Client → Server |
/// | `MapChunkResponse` | Server → Client |
/// | `AnchorPointsEvent` | Server → Client |
/// | `MoveEvent` | Bidirectional (client omits auth `id`; server fills it) |
/// | `SayEvent` | Bidirectional (client omits `position`; server fills it) |
///
/// # Why a single enum rather than `ClientEvent` / `ServerEvent`
///
/// A compile-time split was considered and intentionally declined:
///
/// 1. `SayEvent` and `MoveEvent` are used in both directions with different
///    field semantics — a split requires four structs and a conversion step
///    inside the server broadcast path, adding complexity without safety gain.
/// 2. Every invalid-direction variant is already listed exhaustively in the
///    server's handler and the client's dispatcher and silently discarded.
/// 3. A shared wire enum keeps `send_event`/`recv_event` generic over one type.
///
/// Revisit if a second independent client implementation is ever added.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GameEvent {
    /// First message sent by a client after connecting to register their chosen name.
    RegisterEvent { name: String },

    /// A player said something. The `position` field is authoritative when set by the server.
    SayEvent { position: Option<Position>, text: String },

    /// Sent by the server to a newly connected client to assign its ID and starting position.
    HelloEvent { your_id: u64, start_position: Position },

    /// Sent by the server immediately after `HelloEvent` — full snapshot of all connected players.
    WorldStateEvent { boats: Vec<(u64, Position, String)> },

    /// Broadcast whenever any client moves; `id` identifies who moved.
    MoveEvent { id: u64, position: Position },

    /// Broadcast by the server when a new client joins so existing clients learn the new name.
    NameEvent { id: u64, name: String },

    /// Broadcast by the server when a client disconnects.
    ByeEvent { id: u64 },

    // ── Map protocol ─────────────────────────────────────────────────────────

    /// Sent by the server right after `HelloEvent` to describe the map layout.
    ///
    /// - `tile_width` / `tile_height`: full map size in tiles.
    /// - `chunk_size`: tiles per chunk (chunks are always square).
    /// - `meters_per_tile`: real-world scale factor (informational).
    WorldInfoEvent {
        tile_width: u32,
        tile_height: u32,
        chunk_size: u32,
        meters_per_tile: f32,
    },

    /// Sent by the client to request a single map chunk by its chunk-grid coordinates.
    MapChunkRequest { chunk_x: u32, chunk_y: u32 },

    /// Server response to a `MapChunkRequest`.
    ///
    /// `data` is `chunk_size × chunk_size` [`Tile`] values, row-major (row 0 = northernmost).
    MapChunkResponse {
        chunk_x: u32,
        chunk_y: u32,
        data: Vec<Tile>,
    },

    /// Low-resolution overview of the entire map, sent during the handshake.
    ///
    /// `data` is `width × height` [`Tile`] values, row-major (row 0 = northernmost).
    OverviewMapEvent {
        width: u32,
        height: u32,
        data: Vec<Tile>,
    },

    /// Favourite anchoring points loaded from the server's CSV, sent once during the handshake.
    ///
    /// Positions are in tile-space; clients may render them on the overview map
    /// and track which have been visited during the session.
    AnchorPointsEvent { points: Vec<AnchorPoint> },

    /// Sent by the server immediately on WebSocket connection, before `RegisterEvent`, to identify the server version.
    ///
    /// `version` is the server binary's `CARGO_PKG_VERSION` semver string (e.g. `"0.2.1"`).
    /// Clients use this to detect protocol incompatibility and prompt users to upgrade.
    ///
    /// **Must remain the last variant** — bincode encodes enum variants by index, so
    /// appending here preserves all existing discriminants.
    ServerVersionEvent { version: String },
}
