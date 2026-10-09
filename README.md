# mclanrebroadcast
(MC LAN Re-broadcast)

A proxy to broadcast a remote Minecraft Java server to local devices over LAN so it shows up under the ingame LAN games list.

## Setup

Create a `.env` file in the root directory:

```env
SERVER_ADDRESS=your.server.ip:25565
```

## Usage

```bash
cargo run --release
```
