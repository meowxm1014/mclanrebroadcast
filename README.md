# mclanrebroadcast
(MC LAN Re-broadcast)

A proxy to broadcast a remote Minecraft Java server to local devices over LAN so it shows up under the ingame LAN games list.

## Setup

Create a `config.json` file in the root directory (or run the app to generate a default one):

```json
{
  "server_address": "your.server.ip:25565",
  "local_listen_port": 25565,
  "motd": "Join Me!",
  "multicast_addr": "224.0.2.60:4445",
  "referrer_string": "lan_referrer",
  "max_concurrent_clients": 32,
  "connect_timeout_seconds": 10
}
```

### Configuration Options

| Option | Type | Default | Description |
|---|---|---|---|
| `server_address` | string | `"127.0.0.1:25565"` | Remote Minecraft server destination (`ip:port`). |
| `local_listen_port` | u16 | `25565` | Local TCP port for clients to connect to. |
| `motd` | string | `"Join Me!"` | Message of the Day shown in LAN game list. |
| `multicast_addr` | string | `"224.0.2.60:4445"` | Minecraft LAN discovery UDP multicast address. |
| `referrer_string` | string | `"lan_referrer"` | Referrer tag injected into player join handshake (`host//referrer`). |
| `max_concurrent_clients` | usize | `32` | Maximum concurrent player connections. |
| `connect_timeout_seconds` | u64 | `10` | Timeout connecting to the remote server. |

## Usage

```bash
cargo run --release
```

## Optional server side reader

The proxy will inject the `referrer_string` onto the server address which can be read by the server to track proxied joins on the server side.