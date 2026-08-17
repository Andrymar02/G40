use common::framing::{read_message, send_message};
use common::{ClientMessage, ServerMessage};
use std::io;
use tokio::net::TcpStream;

#[tokio::main]
async fn main() -> io::Result<()> {
    let mut stream = TcpStream::connect("127.0.0.1:8080").await?;

    send_message(
        &mut stream,
        &ClientMessage::Login {
            username: "test1".into(),
            password: "pw".into(),
        },
    )
    .await?;

    let resp: ServerMessage = read_message(&mut stream).await?;
    println!("{:?}", resp);
    println!("diocane");

    send_message(&mut stream, &ClientMessage::PositionUpdate { lat: 6.7, lon: 4.20, timestamp: 6767 }).await?;

    Ok(())
}
