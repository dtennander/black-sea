use anyhow::Result;
use mmo_term::{GameEvent, Position, recv_event, send_event};
use tokio::net::TcpStream;

#[tokio::main]
async fn main() -> Result<()> {
    println!("Running client");
    let mut stream = TcpStream::connect("127.0.0.1:7456").await?;

    send_event(
        &mut stream,
        &GameEvent::SayEvent {
            position: Position { x: 1.0, y: 2.0 },
            text: "Hello, world!".into(),
        },
    )
    .await?;
    loop {
        let event = recv_event(&mut stream).await?;
        if let Some(event) = event {
            match event {
                GameEvent::SayEvent { position, text } => {
                    println!("[SAY] ({}, {}): {}", position.x, position.y, text);
                }
            }
        }
    }
}
