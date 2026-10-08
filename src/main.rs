use dotenvy::dotenv;
use std::env;
use std::sync::Arc;
use tokio::io;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Semaphore;
use tokio::time::{sleep, timeout, Duration};

const MULTICAST_ADDR: &str = "224.0.2.60:4445";
const LOCAL_LISTEN_PORT: u16 = 25565;
const MOTD: &str = "Join Me!";
const MAX_CONCURRENT_CLIENTS: usize = 32;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::main]
async fn main() -> io::Result<()> {
    dotenv().ok();

    let server_address: Arc<str> = env::var("SERVER_ADDRESS")
        .expect("SERVER_ADDRESS must be in .env")
        .into();

    tokio::spawn(async move {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .expect("Failed to bind UDP socket");
        let _ = socket.set_multicast_ttl_v4(1);

        let payload = format!("[MOTD]{MOTD}[/MOTD][AD]{LOCAL_LISTEN_PORT}[/AD]");
        let bytes = payload.as_bytes();

        loop {
            let _ = socket.send_to(bytes, MULTICAST_ADDR).await;
            sleep(Duration::from_millis(1500)).await;
        }
    });

    let listener = TcpListener::bind(format!("0.0.0.0:{LOCAL_LISTEN_PORT}")).await?;
    println!("Listening on :{LOCAL_LISTEN_PORT} -> forwarding to {server_address}");

    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_CLIENTS));

    loop {
        let (mut client, _) = match listener.accept().await {
            Ok(conn) => conn,
            Err(err) => {
                eprintln!("Accept error: {err}");
                sleep(Duration::from_millis(100)).await;
                continue;
            }
        };

        let remote = Arc::clone(&server_address);
        let permit = match semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                eprintln!("Rejected connection: max concurrent clients reached");
                continue;
            }
        };

        tokio::spawn(async move {
            let _permit = permit;
            let _ = client.set_nodelay(true);

            let connect_result = timeout(CONNECT_TIMEOUT, TcpStream::connect(&*remote)).await;
            if let Ok(Ok(mut server)) = connect_result {
                let _ = server.set_nodelay(true);
                let _ = io::copy_bidirectional(&mut client, &mut server).await;
            }
        });
    }
}