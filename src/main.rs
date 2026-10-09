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

const MAX_STRING_LEN: usize = 32767;

fn read_varint(buf: &[u8], offset: &mut usize) -> Option<i32> {
    let mut result = 0i32;
    let mut shift = 0;
    while *offset < buf.len() {
        let byte = buf[*offset];
        *offset += 1;
        result |= ((byte & 0x7F) as i32) << shift;
        if (byte & 0x80) == 0 {
            return Some(result);
        }
        shift += 7;
        if shift >= 35 {
            return None;
        }
    }
    None
}

fn read_string<'a>(buf: &'a [u8], offset: &mut usize) -> Option<&'a str> {
    let mut temp_offset = *offset;
    let raw_len = read_varint(buf, &mut temp_offset)?;
    let len = usize::try_from(raw_len).ok()?;
    if len > MAX_STRING_LEN {
        return None;
    }

    let end = temp_offset.checked_add(len)?;
    if end <= buf.len() {
        let s = std::str::from_utf8(&buf[temp_offset..end]).ok()?;
        *offset = end;
        Some(s)
    } else {
        None
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&next_c) = chars.peek() {
                    chars.next();
                    if next_c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out
}

fn sanitize_player_name(name: &str) -> String {
    let stripped = strip_ansi(name);
    let clean: String = stripped
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(16)
        .collect();
    if clean.is_empty() {
        "unknown".to_string()
    } else {
        clean
    }
}

fn parse_client_intent(buf: &[u8]) -> Option<String> {
    let mut offset = 0;
    let _packet_len = read_varint(buf, &mut offset)?;
    let packet_id = read_varint(buf, &mut offset)?;
    if packet_id != 0 {
        return None;
    }
    let _protocol_version = read_varint(buf, &mut offset)?;
    let _server_addr = read_string(buf, &mut offset)?;
    let port_end = offset.checked_add(2)?;
    if port_end > buf.len() {
        return None;
    }
    offset = port_end; // skip port (u16)
    let next_state = read_varint(buf, &mut offset)?;

    match next_state {
        1 => Some("server list ping".to_string()),
        2 => {
            let mut player_desc = "player login".to_string();
            if offset < buf.len() {
                let mut login_offset = offset;
                if let (Some(_login_len), Some(login_id)) = (
                    read_varint(buf, &mut login_offset),
                    read_varint(buf, &mut login_offset),
                ) {
                    if login_id == 0 {
                        if let Some(name) = read_string(buf, &mut login_offset) {
                            player_desc = format!("player: {}", sanitize_player_name(name));
                        }
                    }
                }
            }
            Some(player_desc)
        }
        _ => None,
    }
}

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
        let (mut client, client_addr) = match listener.accept().await {
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
                eprintln!("Rejected connection from {client_addr}: max concurrent clients reached");
                continue;
            }
        };

        tokio::spawn(async move {
            let _permit = permit;
            let _ = client.set_nodelay(true);

            let mut peek_buf = [0u8; 512];
            let info = match timeout(Duration::from_millis(100), client.peek(&mut peek_buf)).await {
                Ok(Ok(n)) if n > 0 => {
                    let mut parsed = parse_client_intent(&peek_buf[..n]);
                    if parsed.as_deref() == Some("player login") {
                        sleep(Duration::from_millis(50)).await;
                        if let Ok(n2) = client.peek(&mut peek_buf).await {
                            if let Some(refined) = parse_client_intent(&peek_buf[..n2]) {
                                parsed = Some(refined);
                            }
                        }
                    }
                    parsed
                }
                _ => None,
            };

            match info {
                Some(desc) => println!("Client connected: {client_addr} ({desc})"),
                None => println!("Client connected: {client_addr}"),
            }

            match timeout(CONNECT_TIMEOUT, TcpStream::connect(&*remote)).await {
                Ok(Ok(mut server)) => {
                    let _ = server.set_nodelay(true);
                    let _ = io::copy_bidirectional(&mut client, &mut server).await;
                    println!("Client disconnected: {client_addr}");
                }
                Ok(Err(err)) => {
                    eprintln!("Failed to connect to remote server for {client_addr}: {err}");
                }
                Err(_) => {
                    eprintln!("Connection to remote server timed out for {client_addr}");
                }
            }
        });
    }
}