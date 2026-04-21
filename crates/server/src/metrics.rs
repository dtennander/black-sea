use anyhow::Result;
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use lazy_static::lazy_static;
use prometheus::{Encoder, IntCounter, IntGauge, Registry, TextEncoder};

lazy_static! {
    pub static ref REGISTRY: Registry = Registry::new();

    pub static ref ACTIVE_CONNECTIONS: IntGauge = {
        let g = IntGauge::new(
            "blacksea_active_connections",
            "Current number of connected clients",
        )
        .unwrap();
        REGISTRY.register(Box::new(g.clone())).unwrap();
        g
    };

    pub static ref TOTAL_CONNECTIONS: IntCounter = {
        let c = IntCounter::new(
            "blacksea_total_connections_total",
            "All-time TCP connections accepted",
        )
        .unwrap();
        REGISTRY.register(Box::new(c.clone())).unwrap();
        c
    };

    pub static ref MOVES_TOTAL: IntCounter = {
        let c = IntCounter::new("blacksea_moves_total", "Valid MoveEvents processed").unwrap();
        REGISTRY.register(Box::new(c.clone())).unwrap();
        c
    };

    pub static ref INVALID_MOVES_TOTAL: IntCounter = {
        let c = IntCounter::new(
            "blacksea_invalid_moves_total",
            "MoveEvents dropped due to passability check",
        )
        .unwrap();
        REGISTRY.register(Box::new(c.clone())).unwrap();
        c
    };

    pub static ref CHAT_MESSAGES_TOTAL: IntCounter = {
        let c =
            IntCounter::new("blacksea_chat_messages_total", "SayEvents processed").unwrap();
        REGISTRY.register(Box::new(c.clone())).unwrap();
        c
    };

    pub static ref MAP_CHUNK_REQUESTS_TOTAL: IntCounter = {
        let c = IntCounter::new(
            "blacksea_map_chunk_requests_total",
            "MapChunkRequests processed",
        )
        .unwrap();
        REGISTRY.register(Box::new(c.clone())).unwrap();
        c
    };

    pub static ref BROADCAST_LAG_TOTAL: IntCounter = {
        let c = IntCounter::new(
            "blacksea_broadcast_lag_total",
            "Total messages dropped due to broadcast channel lag",
        )
        .unwrap();
        REGISTRY.register(Box::new(c.clone())).unwrap();
        c
    };

    /// Accumulated nautical miles sailed, stored as milli-NM (divide by 1000 in Grafana).
    pub static ref NAUTICAL_MILES_SAILED: IntCounter = {
        let c = IntCounter::new(
            "blacksea_nautical_miles_sailed_milli_total",
            "Total nautical miles sailed across all players (in milli-NM; divide by 1000 for NM)",
        )
        .unwrap();
        REGISTRY.register(Box::new(c.clone())).unwrap();
        c
    };
}

pub fn init() {
    lazy_static::initialize(&REGISTRY);
    lazy_static::initialize(&ACTIVE_CONNECTIONS);
    lazy_static::initialize(&TOTAL_CONNECTIONS);
    lazy_static::initialize(&MOVES_TOTAL);
    lazy_static::initialize(&INVALID_MOVES_TOTAL);
    lazy_static::initialize(&CHAT_MESSAGES_TOTAL);
    lazy_static::initialize(&MAP_CHUNK_REQUESTS_TOTAL);
    lazy_static::initialize(&BROADCAST_LAG_TOTAL);
    lazy_static::initialize(&NAUTICAL_MILES_SAILED);
}

pub async fn serve_metrics(addr: &str) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("[metrics] Listening on {addr}");

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        tokio::spawn(async move {
            let _ = http1::Builder::new()
                .serve_connection(
                    io,
                    service_fn(|_req: Request<hyper::body::Incoming>| async {
                        let encoder = TextEncoder::new();
                        let metric_families = REGISTRY.gather();
                        let mut body = Vec::new();
                        encoder
                            .encode(&metric_families, &mut body)
                            .unwrap_or_default();
                        Ok::<Response<Full<Bytes>>, std::convert::Infallible>(
                            Response::builder()
                                .header("Content-Type", encoder.format_type())
                                .body(Full::new(Bytes::from(body)))
                                .unwrap(),
                        )
                    }),
                )
                .await;
        });
    }
}
