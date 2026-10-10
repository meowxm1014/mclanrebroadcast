use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self as std_io, BufRead};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Semaphore;
use tokio::time::{sleep, timeout, Duration};

const MAX_STRING_LEN: usize = 32767;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    pub server_address: String,
    #[serde(default = "default_local_listen_port")]
    pub local_listen_port: u16,
    #[serde(default = "default_motd")]
    pub motd: String,
    #[serde(default = "default_multicast_addr")]
    pub multicast_addr: String,
    #[serde(default = "default_referrer_string")]
    pub referrer_string: String,
    #[serde(default = "default_max_concurrent_clients")]
    pub max_concurrent_clients: usize,
    #[serde(default = "default_connect_timeout_seconds")]
    pub connect_timeout_seconds: u64,
}

fn default_local_listen_port() -> u16 {
    25565
}

fn default_motd() -> String {
    "Join Me!".to_string()
}

fn default_multicast_addr() -> String {
    "224.0.2.60:4445".to_string()
}

fn default_referrer_string() -> String {
    "lan_referrer".to_string()
}

fn default_max_concurrent_clients() -> usize {
    32
}

fn default_connect_timeout_seconds() -> u64 {
    10
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server_address: "127.0.0.1:25565".to_string(),
            local_listen_port: default_local_listen_port(),
            motd: default_motd(),
            multicast_addr: default_multicast_addr(),
            referrer_string: default_referrer_string(),
            max_concurrent_clients: default_max_concurrent_clients(),
            connect_timeout_seconds: default_connect_timeout_seconds(),
        }
    }
}

// for incomplete server address in config
pub fn prompt_server_address<R: BufRead, W: std_io::Write>(
    mut reader: R,
    mut writer: W,
) -> std_io::Result<String> {
    loop {
        write!(writer, "Enter remote server address (IP or domain:port): ")?;
        writer.flush()?;

        let mut input = String::new();
        if reader.read_line(&mut input)? == 0 {
            return Err(std_io::Error::new(
                std_io::ErrorKind::UnexpectedEof,
                "No input provided",
            ));
        }
        let trimmed = input.trim();
        if trimmed.is_empty() {
            writeln!(
                writer,
                "Address cannot be empty. Please enter a valid server address (e.g. 1.2.3.4:25565)."
            )?;
            continue;
        }

        let formatted = if !trimmed.contains(':') {
            format!("{trimmed}:25565")
        } else {
            trimmed.to_string()
        };

        return Ok(formatted);
    }
}

impl Config {
    pub fn load(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        if !Path::new(path).exists() {
            println!("'{path}' not found.");

            let example_path = "config.example.json";
            if Path::new(example_path).exists() {
                fs::copy(example_path, path)?;
                println!("Copied '{example_path}' to '{path}'.");
            } else {
                let default_cfg = Config::default();
                fs::write(path, serde_json::to_string_pretty(&default_cfg)?)?;
            }

            let content = fs::read_to_string(path)?;
            let mut config: Config = serde_json::from_str(&content).unwrap_or_default();

            println!("\nPlease configure your remote Minecraft server destination:");
            let stdin = std_io::stdin();
            let stdout = std_io::stdout();
            let address = prompt_server_address(stdin.lock(), stdout.lock())?;

            config.server_address = address;

            let updated_json = serde_json::to_string_pretty(&config)?;
            fs::write(path, updated_json)?;
            println!("Configuration saved to '{path}'.\n");

            return Ok(config);
        }

        let content = fs::read_to_string(path)?;
        let config: Config = serde_json::from_str(&content)?;
        Ok(config)
    }
}

// for reading Microsoft's variable length ints
fn read_varint(buf: &[u8], offset: &mut usize) -> Option<i32> {
    let mut result = 0i32;
    let mut shift = 0;

    while *offset < buf.len() {
        let byte = buf[*offset];
        *offset += 1;

        // extract 7 data bits and shift them into place
        result |= ((byte & 0x7F) as i32) << shift;

        // if the most significant bit is 0, we're at the end of the number
        if (byte & 0x80) == 0 {
            return Some(result);
        }

        // if not, read the next 7 bits
        shift += 7;
        if shift >= 35 {
            return None; // corrupt packet protection
        }
    }
    None
}

fn write_varint(val: i32, out: &mut Vec<u8>) {
    let mut v = val as u32;
    loop {
        // get lowest 7 bits of the number
        let mut byte = (v & 0x7F) as u8;

        // discard the bits by shifting right
        v >>= 7;

        // if there are still bits left then set the most signinifcant bit to 1
        if v != 0 {
            byte |= 0x80;
        }

        out.push(byte);

        // if nothing is left to encode then we're done
        if v == 0 {
            break;
        }
    }
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

fn write_string(s: &str, out: &mut Vec<u8>) {
    write_varint(s.len() as i32, out);
    out.extend_from_slice(s.as_bytes());
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

#[derive(Debug, PartialEq, Eq)]
struct ParsedHandshake {
    protocol_version: i32,
    server_addr: String,
    server_port: u16,
    next_state: i32,
}

fn parse_handshake(buf: &[u8]) -> Option<ParsedHandshake> {
    let mut offset = 0;
    let _packet_len = read_varint(buf, &mut offset)?;
    let packet_id = read_varint(buf, &mut offset)?;
    if packet_id != 0 {
        return None;
    }
    let protocol_version = read_varint(buf, &mut offset)?;
    let server_addr = read_string(buf, &mut offset)?.to_string();
    let port_end = offset.checked_add(2)?;
    if port_end > buf.len() {
        return None;
    }
    let server_port = u16::from_be_bytes([buf[offset], buf[offset + 1]]);
    offset = port_end;
    let next_state = read_varint(buf, &mut offset)?;
    Some(ParsedHandshake {
        protocol_version,
        server_addr,
        server_port,
        next_state,
    })
}

fn encode_handshake_packet(
    protocol_version: i32,
    server_addr: &str,
    server_port: u16,
    next_state: i32,
) -> Vec<u8> {
    let mut payload = Vec::new();
    write_varint(0, &mut payload); // Packet ID = 0x00
    write_varint(protocol_version, &mut payload);
    write_string(server_addr, &mut payload);
    payload.extend_from_slice(&server_port.to_be_bytes());
    write_varint(next_state, &mut payload);

    let mut packet = Vec::with_capacity(payload.len() + 5);
    write_varint(payload.len() as i32, &mut packet);
    packet.extend_from_slice(&payload);
    packet
}

fn inject_referrer(server_addr: &str, referrer: &str) -> String {
    if referrer.is_empty() {
        return server_addr.to_string();
    }

    if server_addr.ends_with(referrer) {
        return server_addr.to_string();
    }

    let formatted = if referrer.starts_with('/') {
        format!("{server_addr}{referrer}")
    } else {
        format!("{server_addr}//{referrer}")
    };

    if formatted.len() > 255 {
        let mut end = 255;
        while !formatted.is_char_boundary(end) {
            end -= 1;
        }
        formatted[..end].to_string()
    } else {
        formatted
    }
}

fn parse_player_name_from_login(buf: &[u8]) -> Option<String> {
    let mut offset = 0;
    let _packet_len = read_varint(buf, &mut offset)?;
    let packet_id = read_varint(buf, &mut offset)?;
    if packet_id != 0 {
        return None;
    }
    let name = read_string(buf, &mut offset)?;
    Some(sanitize_player_name(name))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Arc::new(Config::load("config.json")?);

    let multicast_config = Arc::clone(&config);
    tokio::spawn(async move {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .expect("Failed to bind UDP socket");
        let _ = socket.set_multicast_ttl_v4(1);

        let payload = format!(
            "[MOTD]{}[/MOTD][AD]{}[/AD]",
            multicast_config.motd, multicast_config.local_listen_port
        );
        let bytes = payload.as_bytes();

        loop {
            let _ = socket
                .send_to(bytes, &multicast_config.multicast_addr)
                .await;
            sleep(Duration::from_millis(1500)).await;
        }
    });

    let listener =
        TcpListener::bind(format!("0.0.0.0:{}", config.local_listen_port)).await?;
    println!(
        "Listening on :{} -> forwarding to {}",
        config.local_listen_port, config.server_address
    );

    let semaphore = Arc::new(Semaphore::new(config.max_concurrent_clients));
    let connect_timeout = Duration::from_secs(config.connect_timeout_seconds);

    loop {
        let (mut client, client_addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(err) => {
                eprintln!("Accept error: {err}");
                sleep(Duration::from_millis(100)).await;
                continue;
            }
        };

        let conn_config = Arc::clone(&config);
        let permit = match semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                eprintln!("Rejected connection from {}: max concurrent clients reached", client_addr);
                continue;
            }
        };

        tokio::spawn(async move {
            let _permit = permit;
            let _ = client.set_nodelay(true);

            let mut client_buf = Vec::with_capacity(512);
            let mut temp_buf = [0u8; 512];

            let handshake_read = timeout(Duration::from_secs(6), async {
                loop {
                    let n = client.read(&mut temp_buf).await?;
                    if n == 0 {
                        return Ok::<(), io::Error>(());
                    }
                    client_buf.extend_from_slice(&temp_buf[..n]);

                    let mut offset = 0;
                    if let Some(packet_len) = read_varint(&client_buf, &mut offset) {
                        if packet_len > 0 {
                            let total_len = offset + (packet_len as usize);
                            if client_buf.len() >= total_len {
                                break;
                            }
                        }
                    }
                }
                Ok(())
            })
            .await;

            if let Err(_) | Ok(Err(_)) = handshake_read {
                eprintln!("Failed to read handshake from {client_addr}");
                return;
            }

            if client_buf.is_empty() {
                return;
            }

            let mut offset = 0;
            let first_packet_len = match read_varint(&client_buf, &mut offset) {
                Some(len) if len > 0 => offset + (len as usize),
                _ => client_buf.len(),
            };

            let (outgoing_handshake, leftover_bytes) = if first_packet_len <= client_buf.len() {
                let first_packet = &client_buf[..first_packet_len];
                if let Some(handshake) = parse_handshake(first_packet) {
                    if handshake.next_state == 2 {

                        if client_buf.len() == first_packet_len {
                            let _ = timeout(Duration::from_millis(50), async {
                                if let Ok(n) = client.read(&mut temp_buf).await {
                                    if n > 0 {
                                        client_buf.extend_from_slice(&temp_buf[..n]);
                                    }
                                }
                            })
                            .await;
                        }

                        let player_desc = parse_player_name_from_login(&client_buf[first_packet_len..])
                            .map(|name| format!("player: {name}"))
                            .unwrap_or_else(|| "player login".to_string());

                        let new_addr =
                            inject_referrer(&handshake.server_addr, &conn_config.referrer_string);
                        println!("Client connected: {client_addr} ({player_desc})");

                        let rewritten = encode_handshake_packet(
                            handshake.protocol_version,
                            &new_addr,
                            handshake.server_port,
                            handshake.next_state,
                        );

                        (rewritten, client_buf[first_packet_len..].to_vec())
                    } else {
                        if handshake.next_state == 1 {
                            println!("Client connected: {client_addr} (server list ping)");
                        } else {
                            println!(
                                "Client connected: {client_addr} (state {})",
                                handshake.next_state
                            );
                        }
                        (first_packet.to_vec(), client_buf[first_packet_len..].to_vec())
                    }
                } else {
                    println!("Client connected: {client_addr} (non-handshake data)");
                    (client_buf, Vec::new())
                }
            } else {
                println!("Client connected: {client_addr}");
                (client_buf, Vec::new())
            };

            match timeout(connect_timeout, TcpStream::connect(&conn_config.server_address)).await {
                Ok(Ok(mut server)) => {
                    let _ = server.set_nodelay(true);
                    if let Err(err) = server.write_all(&outgoing_handshake).await {
                        eprintln!(
                            "Failed to write handshake to remote server for {client_addr}: {err}"
                        );
                        return;
                    }
                    if !leftover_bytes.is_empty() {
                        if let Err(err) = server.write_all(&leftover_bytes).await {
                            eprintln!(
                                "Failed to write initial payload to remote server for {client_addr}: {err}"
                            );
                            return;
                        }
                    }
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