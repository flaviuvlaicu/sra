# SRA — How It Works

SRA (Secure Remote Access) is a self-hosted system for reaching servers behind NAT/firewalls without SSH, without open inbound ports, and with optional end-to-end encryption. It is a fork of [Narrowlink](https://github.com/narrowlink/narrowlink).

---

## Architecture Overview

```
Operator (Nostrom)
  |
  sra shell -n sramon -k <passphrase>
  |
  └── spawns: sra connect -n sramon -k <passphrase> 127.0.0.1 22222
       |
       └── WSS + XChaCha20-Poly1305 ──► gw.sra.sh:443 (Cloudflare edge)
            |
            └── Cloudflare Tunnel ──► sra-gateway (VPS, localhost:80)
                 |
                 └── WSS + XChaCha20-Poly1305 ──► sra-agent (target server)
                      |
                      └── TCP 127.0.0.1:22222 ──► PTY shell (/bin/bash)
```

There are four binaries:

| Binary | Role | Runs on |
|--------|------|---------|
| `sra-gateway` | WebSocket relay server | VPS behind Cloudflare Tunnel |
| `sra-agent` | Outbound connector + PTY shell server | Every target server |
| `sra` | Operator CLI (list, connect, forward, proxy, shell) | Operator's machine |
| `sra-token` | JWT token generator | VPS (offline tool) |

---

## Step-by-Step: What Happens When You Run `sra shell`

### Step 1: Operator runs `sra shell -n sramon -k <passphrase>`

The `sra` binary parses the `shell` subcommand in `sra-client/src/args.rs`. It recognizes `-n` (agent name) and `-k` (E2E encryption passphrase). If `-k` is not provided, it checks the `SRA_E2EE_KEY` environment variable.

The shell handler in `sra-client/src/main.rs` intercepts the `Shell` command *before* the normal async runtime starts and calls `shell::run_shell()`.

### Step 2: Client spawns `sra connect` as a subprocess

`sra-client/src/shell.rs` spawns itself as a child process:

```
sra connect -n sramon -k <passphrase> 127.0.0.1 22222
```

This creates a TCP tunnel from the operator through the gateway to port 22222 on the agent (the PTY shell server). The child process's stdin/stdout are piped — the shell module talks to the tunnel through these pipes using a simple framing protocol:

- **MSG_DATA (0x00)**: `[0x00] [length: u16 BE] [payload]`
- **MSG_RESIZE (0x01)**: `[0x01] [cols: u16 BE] [rows: u16 BE]`

The client sends the initial terminal dimensions (cols, rows as u16 BE) immediately after the pipe opens.

### Step 3: `sra connect` establishes a WebSocket tunnel to the gateway

The connect command (existing Narrowlink logic) does:

1. Loads config from `~/.sra/client.yaml` or `/etc/sra/client.yaml`
2. Connects to the gateway via WebSocket (WSS through Cloudflare)
3. Authenticates with a JWT token (sent as `NL-TOKEN` header)
4. Requests a connection to agent `sramon`, destination `127.0.0.1:22222`
5. If `-k` was provided, derives an XChaCha20-Poly1305 key:
   - Generate a random 24-byte nonce
   - `key = SHA3-256(passphrase XOR nonce)`
   - HMAC-SHA256 signature verifies both sides share the same passphrase
   - All tunnel data is encrypted end-to-end (gateway sees only ciphertext)

### Step 4: Gateway relays the connection to the agent

`sra-gateway` runs on the VPS listening on `localhost:80`. Cloudflare Tunnel (`cloudflared`) handles TLS termination and forwards traffic.

The gateway:
1. Validates the client's JWT token
2. Looks up agent `sramon` in its connected agents table
3. Sends a `Connect` event to the agent over its persistent event WebSocket
4. The agent opens a new data WebSocket back to the gateway
5. Gateway bridges the client's data WebSocket to the agent's data WebSocket

The gateway never sees plaintext if E2E encryption is enabled.

### Step 5: Agent receives the connection request

`sra-agent` maintains a persistent outbound WebSocket to the gateway (no inbound ports needed). When it receives a `Connect` event for `127.0.0.1:22222`:

1. Opens a TCP connection to `127.0.0.1:22222` (the local PTY shell server)
2. Opens a data WebSocket back to the gateway
3. If E2E encryption is active, wraps the data stream with XChaCha20-Poly1305
4. Forwards bytes bidirectionally between the TCP socket and the data WebSocket

### Step 6: Agent PTY shell server handles the session

`sra-agent/src/shell.rs` runs a TCP listener on `127.0.0.1:22222` (started at agent boot via `tokio::spawn(shell::start_shell_server())`).

When a TCP connection arrives:

1. Reads initial terminal size (cols, rows as u16 BE)
2. Opens a PTY (`portable-pty`) with the detected shell (`$SHELL`, `/bin/bash`, or `/bin/sh`)
3. Sets `TERM=xterm-256color`
4. Spawns the shell process attached to the PTY slave
5. Creates two OS threads (captured tokio runtime handle for async I/O):
   - **Reader thread**: Reads PTY output → frames as MSG_DATA → writes to TCP
   - **Writer thread**: Receives MSG_DATA/MSG_RESIZE from a channel → writes to PTY / resizes PTY
6. An async task reads TCP frames and sends them to the writer thread via an mpsc channel
7. When the session ends, the child process is waited on to prevent zombies

### Step 7: Operator interacts with the remote shell

Back on the operator's machine, `sra-client/src/shell.rs`:

1. Puts the terminal in raw mode (crossterm)
2. **Output thread**: Reads framed MSG_DATA from the subprocess stdout pipe → writes raw bytes to local stdout
3. **Main thread**: Polls keyboard events (crossterm) at 50ms intervals:
   - Converts key events to terminal byte sequences (`key_to_bytes`)
   - Frames them as MSG_DATA and writes to the subprocess stdin pipe
   - Terminal resize events are sent as MSG_RESIZE
4. `Ctrl+]` exits the session (telnet-style escape)
5. On exit: restores terminal mode, closes stdin pipe, kills subprocess

---

## Agent Retry Logic

The agent (`sra-agent/src/main.rs`) connects outbound to the gateway and retries on failure:

- **Backoff**: 0s, 10s, 20s, 30s, 40s, 50s, 60s, 60s, 60s... (capped at 60s)
- **Only permanent exit**: HTTP 401 (authentication failed — bad token)
- **Retries forever on**: Network errors, timeouts, HTTP 403, any other transient failure
- The upstream Narrowlink bug (exit after 70s) has been fixed

---

## E2E Encryption Flow

When `-k <passphrase>` or `SRA_E2EE_KEY` is set:

1. Client generates a random 24-byte nonce
2. Both client and agent derive: `key = SHA3-256(passphrase XOR nonce)`
3. Client signs the connection metadata with `HMAC-SHA256(key, host:port:protocol || nonce)`
4. Agent verifies the HMAC — if it fails, the passphrase doesn't match and the connection is rejected
5. Both sides wrap the data stream with `XChaCha20-Poly1305(key, nonce)`
6. The gateway relays ciphertext it cannot read

If the agent config has `policy: Strict`, unencrypted connections are rejected.

---

## Token Authentication

Tokens are JWTs signed with HMAC-SHA256 using a shared secret between the gateway and the token generator.

```bash
sra-token -c /etc/sra/token-gen.yaml
```

The config specifies:
- A shared `secret` (byte array, must match `gateway.yaml`)
- Agent tokens: `uid`, `name`, `exp` (expiration timestamp)
- Client tokens: `uid`, `name`, `exp`

The gateway validates tokens on every WebSocket upgrade request.

---

## Network Topology

```
Internet-facing:    Cloudflare edge (gw.sra.sh:443, TLS termination)
                         |
                    Cloudflare Tunnel (encrypted)
                         |
VPS (no public ports):  sra-gateway (localhost:80, plain HTTP)
                         |
                    Outbound WSS from agents
                         |
Target servers:     sra-agent (behind NAT/firewall, no inbound ports)
                    └── PTY shell on localhost:22222
```

Key security properties:
- **VPS IP hidden**: Cloudflare Tunnel means no DNS A record pointing to the VPS
- **No inbound ports on agents**: Agents connect outbound to the gateway
- **PTY isolated**: Shell server binds to `127.0.0.1` only — unreachable from the network
- **E2E encryption optional**: XChaCha20-Poly1305 when passphrase is configured
- **Config protection**: All secret-containing files are `chmod 600`

---

## File Structure

```
sra/
├── Cargo.toml                  workspace root
├── CLAUDE.md                   build plan / architecture reference
├── sra-core/                   shared types (JWT tokens, policies, events)
│   └── src/
│       ├── lib.rs              re-exports
│       ├── generic.rs          Connect, Protocol, HmacSha256
│       ├── token.rs            JWT token types
│       ├── policy.rs           IP/host access policies
│       ├── agent/              agent event types (inbound/outbound)
│       └── client/             client event types
├── sra-network/                network layer (WebSocket, QUIC, encryption)
│   └── src/
│       ├── lib.rs              async_forward, AsyncSocket, AsyncSocketCrypt
│       ├── ws.rs               WebSocket connection (WsConnection, WsConnectionBinary)
│       ├── transport.rs        TLS/TCP/UDP unified socket
│       ├── event.rs            NarrowEvent (typed event stream over WebSocket)
│       ├── p2p.rs              QUIC peer-to-peer (optional direct connections)
│       └── async_tools.rs      chunked I/O utilities
├── sra-gateway/                relay server
│   └── src/
│       ├── main.rs             entry point
│       ├── config.rs           gateway.yaml parsing
│       ├── service/
│       │   ├── ws.rs           WebSocket handler (agent/client auth + bridging)
│       │   ├── wss.rs          TLS WebSocket handler
│       │   └── certificate/    ACME cert management
│       └── state/
│           ├── mod.rs          connection state machine
│           ├── agent.rs        agent session tracking
│           ├── client.rs       client session tracking
│           └── connection.rs   data channel bridging
├── sra-agent/
│   └── src/
│       ├── main.rs             entry point, retry logic, data forwarding
│       ├── shell.rs            PTY shell server on localhost:22222
│       ├── config.rs           agent.yaml parsing (endpoints, E2EE)
│       └── args.rs             CLI argument parsing
├── sra-client/
│   └── src/
│       ├── main.rs             entry point, shell early-exit
│       ├── shell.rs            interactive shell session (raw terminal, framing)
│       ├── args.rs             CLI parsing (forward, list, connect, proxy, tun, shell)
│       ├── manage.rs           control channel management
│       ├── transport.rs        relay/direct transport selection
│       ├── tunnel/
│       │   ├── mod.rs          tunnel factory (forward, connect, proxy, tun)
│       │   ├── input_stream.rs stdin/stdout streaming for connect mode
│       │   └── tun.rs          TUN device support (Linux/macOS/Windows)
│       └── config.rs           client.yaml parsing
├── sra-token/
│   └── src/
│       ├── main.rs             token generation entry point
│       ├── config.rs           token-gen.yaml parsing
│       └── args.rs             CLI parsing
├── docs/
│   ├── how-it-works.md         this document
│   └── e2e-encryption.md       E2E encryption setup guide
├── scripts/
│   └── install-agent.sh        one-line agent installer
└── .github/
    └── workflows/
        └── ci.yml              cross-platform CI + release workflow
```

---

## Command Reference

```bash
# List connected agents
sra list
sra list -v                    # verbose (shows system info)

# Interactive shell (recommended)
sra shell -n <agent>                     # TLS only
sra shell -n <agent> -k <passphrase>     # E2E encrypted
SRA_E2EE_KEY=<passphrase> sra shell -n <agent>  # via env var

# Raw TCP tunnel
sra connect -n <agent> <host> <port>

# Port forwarding
sra forward -n <agent> <remote_host:port>
sra forward -n <agent> -l 127.0.0.1:8080 <remote_host:port>

# SOCKS5 proxy
sra proxy -n <agent>
sra proxy -n <agent> 127.0.0.1:9090

# Token generation
sra-token -c /etc/sra/token-gen.yaml
```
