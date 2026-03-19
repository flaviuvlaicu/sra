# SRA — Build Plan for Claude Code

## What is SRA

SRA is a self-hosted secure remote access system forked from Narrowlink. It allows
agents deployed on servers behind NAT/firewalls to connect outbound to a relay gateway,
and operators to reach those agents through an encrypted tunnel with a built-in PTY shell.

No SSH required. No open inbound ports on agents. No exposed VPS IP (Cloudflare Tunnel).

## Architecture

```
Nostrom (operator)
  └─ sra shell -n sramon
       └─ sra connect -n sramon -k <passphrase> 127.0.0.1 22222
            └─ WSS + XChaCha20-Poly1305 → gw.sra.sh:443 (Cloudflare edge)
                 └─ Cloudflare Tunnel → sra-gateway (VPS, sees only ciphertext)
                      └─ WSS + XChaCha20-Poly1305 → sra-agent (sramon, outbound only)
                           └─ PTY → /bin/bash
```

## Repository structure to produce

```
sra/
├── CLAUDE.md
├── Cargo.toml                        ← workspace root
├── sra-core/                         ← shared types (renamed from narrowlink-common/types)
├── sra-network/                      ← network layer (renamed from narrowlink-network)
├── sra-gateway/                      ← relay server
├── sra-agent/
│   └── src/
│       ├── main.rs                   ← MODIFIED: fix exit bug, spawn shell server
│       └── shell.rs                  ← NEW: PTY server on localhost:22222
├── sra-client/
│   └── src/
│       ├── main.rs                   ← MODIFIED: add Shell arm
│       ├── args.rs                   ← MODIFIED: add Shell to custom clap_lex parser
│       └── shell.rs                  ← NEW: shell session manager
├── sra-token/                        ← token generator
├── scripts/
│   └── install-agent.sh
└── .github/
    └── workflows/
        └── ci.yml
```

---

## Phase 0 — Narrowlink codebase audit (fix before anything else)

These bugs exist in upstream Narrowlink and MUST be fixed.

### Bug 0.1 — Agent permanently exits after ~2 minutes (CRITICAL)

**File:** `sra-agent/src/main.rs`

**Problem:** The retry logic exits after 70 seconds of failures:
```rust
if sleep_time == 70 {
    error!("Unable to connect");
    info!("Exit");
    break;   // permanently exits
}
sleep_time += 10;
```
Retries at 0s, 10s, 20s...70s then stops. Systemd restarts but it's fragile.
We observed this in production — the agent gave up connecting to the gateway.

**Fix:** In the `Err(e)` arm of the gateway connection match, replace the entire
retry block with:
```rust
Err(e) => {
    if let NetworkError::UnableToUpgrade(status) = e {
        match status {
            401 => {
                error!("Authentication failed — check your token");
                break;  // only auth failures are permanent
            }
            403 => {
                error!("Access denied");
            }
            _ => {}
        }
    };
    error!("Unable to connect to the gateway: {}", e.to_string());
    if sleep_time == 0 {
        info!("Try again");
    } else {
        info!("Try again in {} secs", sleep_time);
    }
    time::sleep(Duration::from_secs(sleep_time)).await;
    sleep_time = (sleep_time + 10).min(60);  // cap at 60s, never exit on network errors
}
```

### Bug 0.2 — jsonwebtoken missing crypto provider (CRITICAL)

**Problem:** `jsonwebtoken` v10 panics on first real connection:
```
Could not automatically determine the process-level CryptoProvider
```

**Fix:** Search all Cargo.toml files:
```bash
grep -r "jsonwebtoken" --include="*.toml" .
```

For every match, add `rust_crypto` to features:
```toml
jsonwebtoken = { version = "10.3.0", default-features = false, features = ["use_pem", "rust_crypto"] }
```

### Bug 0.3 — Token generator binary name wrong after rename

**Problem:** `[[bin]] name = "narrowlink-token-generator"` → sed produces
`sra-token-generator` but we want `sra-token`.

**Fix:** After rename, explicitly set in `sra-token/Cargo.toml`:
```toml
[[bin]]
name = "sra-token"
path = "src/main.rs"
```

### Bug 0.4 — Client binary name

**File:** `sra-client/Cargo.toml`

The original is `[[bin]] name = "narrowlink"`. After rename it becomes `sra`.
Verify this is correct:
```toml
[[bin]]
name = "sra"
path = "src/main.rs"
```

### Bug 0.5 — Version strings in help text

After rename, search for hardcoded product names and fix:
```bash
grep -rn "Narrowlink Client\|Narrowlink Agent\|Narrowlink Gateway" --include="*.rs" .
```
Replace with `SRA Client`, `SRA Agent`, `SRA Gateway`.

---

## Phase 1 — Clone and rename Narrowlink

### 1.1 Clone upstream

```bash
git clone https://github.com/narrowlink/narrowlink sra
cd sra
git remote rename origin upstream
```

### 1.2 Check what directories exist

```bash
ls -d narrowlink-*/
```

### 1.3 Rename crate directories (only rename what exists)

```bash
[ -d narrowlink-common ]          && mv narrowlink-common          sra-core
[ -d narrowlink-types ]           && mv narrowlink-types           sra-types
[ -d narrowlink-network ]         && mv narrowlink-network         sra-network
[ -d narrowlink-gateway ]         && mv narrowlink-gateway         sra-gateway
[ -d narrowlink-agent ]           && mv narrowlink-agent           sra-agent
[ -d narrowlink-client ]          && mv narrowlink-client          sra-client
[ -d narrowlink-token-generator ] && mv narrowlink-token-generator sra-token
```

### 1.4 Root Cargo.toml — update workspace members

Replace the `[workspace]` members list with whatever directories exist:
```toml
[workspace]
members = [
    "sra-core",
    "sra-network",
    "sra-gateway",
    "sra-agent",
    "sra-client",
    "sra-token",
]
resolver = "2"
```

### 1.5 Fix all Rust source references

**IMPORTANT: Use perl, not sed. `sed -i` fails on macOS without '' argument.**

```bash
# Fix crate name references (underscored form used in use/extern)
find . -name "*.rs" -not -path "*/target/*" | xargs perl -pi -e '
  s/narrowlink_common\b/sra_core/g;
  s/narrowlink_types\b/sra_core/g;
  s/narrowlink_network\b/sra_network/g;
  s/narrowlink_gateway\b/sra_gateway/g;
  s/narrowlink_agent\b/sra_agent/g;
  s/narrowlink_client\b/sra_client/g;
  s/narrowlink_token\b/sra_token/g;
'

# Fix user-visible strings only (NOT URLs, NOT license text)
find . -name "*.rs" -not -path "*/target/*" | xargs perl -pi -e '
  s/Narrowlink Client/SRA Client/g;
  s/Narrowlink Agent/SRA Agent/g;
  s/Narrowlink Gateway/SRA Gateway/g;
  s/Narrowlink Token/SRA Token/g;
  s/narrowlink-token-generator/sra-token/g;
  s/narrowlink-client/sra-client/g;
  s/narrowlink-agent/sra-agent/g;
  s/narrowlink-gateway/sra-gateway/g;
  s/narrowlink-network/sra-network/g;
  s/narrowlink-common/sra-core/g;
  s/\.narrowlink\b/.sra/g;
  s/"narrowlink"/"sra"/g;
'
```

### 1.6 Fix each crate's Cargo.toml

Edit each Cargo.toml manually. For each:
1. Change `name = "narrowlink-*"` to `name = "sra-*"`
2. Change `[[bin]] name = "narrowlink-*"` to `name = "sra-*"`
3. Fix any `path = "../narrowlink-*"` to `path = "../sra-*"`
4. Fix crate name keys in `[dependencies]` sections

Fix path dependencies across all Cargo.toml files:
```bash
find . -name "Cargo.toml" -not -path "*/target/*" | xargs perl -pi -e '
  s|path = "\.\./narrowlink-common"|path = "../sra-core"|g;
  s|path = "\.\./narrowlink-types"|path = "../sra-core"|g;
  s|path = "\.\./narrowlink-network"|path = "../sra-network"|g;
  s|path = "\.\./narrowlink-gateway"|path = "../sra-gateway"|g;
  s|path = "\.\./narrowlink-agent"|path = "../sra-agent"|g;
  s|path = "\.\./narrowlink-client"|path = "../sra-client"|g;
  s|path = "\.\./narrowlink-token-generator"|path = "../sra-token"|g;
'
```

Fix dep name keys (the left-hand side of dependency entries):
```bash
find . -name "Cargo.toml" -not -path "*/target/*" | xargs perl -pi -e '
  s/^narrowlink-common\s*=/sra-core =/gm;
  s/^narrowlink-types\s*=/sra-core =/gm;
  s/^narrowlink-network\s*=/sra-network =/gm;
'
```

### 1.7 Add new dependencies for shell feature

In `sra-agent/Cargo.toml`, under `[dependencies]`, ADD (do not replace existing):
```toml
portable-pty = "0.8"
anyhow = "1"
```

In `sra-client/Cargo.toml`, under `[dependencies]`, ADD:
```toml
crossterm = "0.27"
anyhow = "1"
```

### 1.8 Verify build

```bash
cargo build --workspace 2>&1 | head -80
```

Fix any remaining errors. Common sources:
```bash
grep -rn "narrowlink" --include="*.rs" --include="*.toml" . \
  | grep -v target/ | grep -v ".git/"
```

---

## Phase 2 — Fix agent exit bug

**File: `sra-agent/src/main.rs`**

Find the retry block — it will look like this BEFORE the fix:
```rust
if sleep_time == 0 {
    info!("Try again");
} else if sleep_time == 70 {
    error!("Unable to connect");
    info!("Exit");
    break;
} else {
    info!("Try again in {} secs", sleep_time);
}
time::sleep(Duration::from_secs(sleep_time)).await;
sleep_time += 10;
```

Replace with:
```rust
if sleep_time == 0 {
    info!("Try again");
} else {
    info!("Try again in {} secs", sleep_time);
}
time::sleep(Duration::from_secs(sleep_time)).await;
sleep_time = (sleep_time + 10).min(60);
```

Keep the `break` only on 401 (auth failure). Remove it from the backoff logic.

---

## Phase 3 — Add PTY shell to sra-agent

### 3.1 Create `sra-agent/src/shell.rs`

```rust
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracing::{error, info};

pub const SHELL_PORT: u16 = 22222;

const MSG_DATA: u8 = 0x00;
const MSG_RESIZE: u8 = 0x01;

enum PtyMsg {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Close,
}

pub async fn start_shell_server() {
    let listener = match TcpListener::bind(format!("127.0.0.1:{}", SHELL_PORT)).await {
        Ok(l) => l,
        Err(e) => {
            error!("SRA shell server failed to bind on port {}: {}", SHELL_PORT, e);
            return;
        }
    };
    info!("SRA shell server listening on 127.0.0.1:{}", SHELL_PORT);
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                info!("New shell session from {}", peer);
                tokio::spawn(async move {
                    if let Err(e) = handle_session(stream).await {
                        error!("Shell session error: {}", e);
                    }
                });
            }
            Err(e) => error!("Shell server accept error: {}", e),
        }
    }
}

async fn handle_session(mut stream: TcpStream) -> anyhow::Result<()> {
    // Client sends initial terminal size: cols (u16 BE) then rows (u16 BE)
    let cols = stream.read_u16().await.unwrap_or(220);
    let rows = stream.read_u16().await.unwrap_or(50);

    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows, cols, pixel_width: 0, pixel_height: 0,
    })?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| {
        if std::path::Path::new("/bin/bash").exists() {
            "/bin/bash".to_string()
        } else {
            "/bin/sh".to_string()
        }
    });

    let mut cmd = CommandBuilder::new(&shell);
    cmd.env("TERM", "xterm-256color");

    let _child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    // Use channels to avoid Arc/Clone issues with PtyMaster
    let (pty_tx, mut pty_rx) = mpsc::channel::<PtyMsg>(64);
    let pty_tx_close = pty_tx.clone();

    let mut master_reader = pair.master.try_clone_reader()?;
    let mut master_writer = pair.master.take_writer()?;
    let master = pair.master;

    let (mut tcp_read, mut tcp_write) = stream.into_split();

    // Blocking thread: owns master for resize, handles input writes
    // Separate sub-thread for PTY reads
    let pty_thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Handle::current();

        // Sub-thread: reads PTY output and sends to TCP
        let reader_thread = std::thread::spawn(move || {
            let rt2 = tokio::runtime::Handle::current();
            let mut buf = [0u8; 4096];
            loop {
                match master_reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let len = n as u16;
                        let mut frame = Vec::with_capacity(3 + n);
                        frame.push(MSG_DATA);
                        frame.extend_from_slice(&len.to_be_bytes());
                        frame.extend_from_slice(&buf[..n]);
                        if rt2.block_on(tcp_write.write_all(&frame)).is_err() { break; }
                        let _ = rt2.block_on(tcp_write.flush());
                    }
                }
            }
        });

        // Process input and resize messages from async side
        while let Some(msg) = rt.block_on(pty_rx.recv()) {
            match msg {
                PtyMsg::Input(data) => {
                    if master_writer.write_all(&data).is_err() { break; }
                }
                PtyMsg::Resize { cols, rows } => {
                    let _ = master.resize(PtySize {
                        rows, cols, pixel_width: 0, pixel_height: 0,
                    });
                }
                PtyMsg::Close => break,
            }
        }
        let _ = reader_thread.join();
    });

    // Async task: reads TCP and routes to PTY channel
    let tcp_to_pty = tokio::spawn(async move {
        let mut msg_type = [0u8; 1];
        loop {
            if tcp_read.read_exact(&mut msg_type).await.is_err() { break; }
            match msg_type[0] {
                MSG_DATA => {
                    let len = match tcp_read.read_u16().await {
                        Ok(n) => n as usize, Err(_) => break,
                    };
                    let mut data = vec![0u8; len];
                    if tcp_read.read_exact(&mut data).await.is_err() { break; }
                    if pty_tx.send(PtyMsg::Input(data)).await.is_err() { break; }
                }
                MSG_RESIZE => {
                    let cols = match tcp_read.read_u16().await { Ok(n) => n, Err(_) => break };
                    let rows = match tcp_read.read_u16().await { Ok(n) => n, Err(_) => break };
                    if pty_tx.send(PtyMsg::Resize { cols, rows }).await.is_err() { break; }
                }
                _ => break,
            }
        }
        let _ = pty_tx.send(PtyMsg::Close).await;
    });

    tcp_to_pty.await?;
    let _ = pty_tx_close.send(PtyMsg::Close).await;
    let _ = tokio::task::spawn_blocking(move || pty_thread.join()).await;

    info!("Shell session ended");
    Ok(())
}
```

### 3.2 Register shell server in `sra-agent/src/main.rs`

Add at the top of the file:
```rust
mod shell;
```

Inside `start()`, before the main `loop {`, add:
```rust
tokio::spawn(shell::start_shell_server());
```

---

## Phase 4 — Add shell subcommand to sra-client

**CRITICAL: The client uses a custom `clap_lex` parser in `args.rs`, NOT clap derive.**
Do not add a `#[derive(Subcommand)]` enum. Follow the existing manual parsing pattern.

### 4.1 Add `ShellArgs` to `sra-client/src/args.rs`

Add after the `ConnectArgs` struct:
```rust
#[derive(Debug, Clone)]
pub struct ShellArgs {
    pub agent_name: String,
    pub cryptography: Option<String>,
}
```

### 4.2 Add `Shell` to `SubCommands` enum

Find the `SubCommands` enum and add `Shell`:
```rust
enum SubCommands {
    Forward,
    List,
    Connect,
    Tun,
    Proxy,
    Shell,   // ← add
}
```

### 4.3 Register "shell" in `SubCommands::new()`

Find the `HashMap::from([...])` with command names. Add `"shell"`:
```rust
("shell", 0),
```

Add the match arm in the final match:
```rust
"shell" => Ok(Self::Shell),
```

### 4.4 Add Shell parsing in `Args::parse()`

Add this case in the `match SubCommands::new(...)` block:
```rust
SubCommands::Shell => {
    let mut sub = ShellArgs {
        agent_name: String::new(),
        cryptography: None,
    };
    while let Some(arg) = raw.next(&mut cursor) {
        if let Some((long, value)) = arg.to_long() {
            match long {
                Ok("name") => {
                    sub.agent_name = value
                        .ok_or(ClientError::RequiredValue("name"))?
                        .to_str()
                        .ok_or(ClientError::Encoding)?
                        .to_string();
                }
                Ok("key") => {
                    sub.cryptography = Some(
                        value
                            .ok_or(ClientError::RequiredValue("key"))?
                            .to_str()
                            .ok_or(ClientError::Encoding)?
                            .to_string(),
                    );
                }
                Ok("help") => {
                    println!("Usage: sra shell -n <agent> [-k <passphrase>]");
                    println!("  -n, --name  Agent to connect to (required)");
                    println!("  -k, --key   E2E encryption passphrase");
                    println!("  Ctrl+] to exit session");
                    println!("  Env: SRA_E2EE_KEY=<passphrase> (avoids shell history)");
                    std::process::exit(0);
                }
                _ => {}
            }
        } else if let Some(mut shorts) = arg.to_short() {
            while let Some(short) = shorts.next_flag() {
                match short {
                    Ok('n') => {
                        sub.agent_name = if let Some(v) = shorts.next_value_os() {
                            v.to_str()
                        } else if let Some(v) = raw.next_os(&mut cursor) {
                            v.to_str().and_then(|v| {
                                if v.is_empty() || v.find('-') == Some(0) { None } else { Some(v) }
                            })
                        } else {
                            None
                        }
                        .ok_or(ClientError::Encoding)?
                        .to_string();
                    }
                    Ok('k') => {
                        let next_value = if let Some(v) = shorts.next_value_os() {
                            v.to_str()
                        } else if let Some(v) = raw.next_os(&mut cursor) {
                            v.to_str().and_then(|v| {
                                if v.is_empty() || v.find('-') == Some(0) { None } else { Some(v) }
                            })
                        } else {
                            None
                        };
                        sub.cryptography = Some(
                            next_value
                                .ok_or(ClientError::RequiredValue("key"))?
                                .to_string(),
                        );
                    }
                    Ok('h') => {
                        println!("Usage: sra shell -n <agent> [-k <passphrase>]");
                        std::process::exit(0);
                    }
                    _ => {}
                }
            }
        }
    }
    if sub.agent_name.is_empty() {
        Err(ClientError::RequiredValue("name"))
    } else {
        Ok(ArgCommands::Shell(sub))
    }
}
```

### 4.5 Add `Shell` to `ArgCommands` enum

```rust
pub enum ArgCommands {
    Forward(ForwardArgs),
    List(ListArgs),
    Proxy(ProxyArgs),
    Connect(ConnectArgs),
    Shell(ShellArgs),   // ← add
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    Tun(TunArgs),
}
```

### 4.6 Create `sra-client/src/shell.rs`

```rust
use crossterm::{
    event::{poll, read, Event, KeyCode, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, size as terminal_size},
};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

const MSG_DATA: u8 = 0x00;
const MSG_RESIZE: u8 = 0x01;
const SHELL_PORT: u16 = 22222;

/// Run an interactive shell session on the named agent.
///
/// passphrase: if Some, passes -k to `sra connect` for XChaCha20-Poly1305 E2E encryption.
/// Also checks SRA_E2EE_KEY env var as fallback (avoids shell history exposure).
pub fn run_shell(agent: &str, passphrase: Option<&str>) -> anyhow::Result<()> {
    let env_key = std::env::var("SRA_E2EE_KEY").ok();
    let effective_key = passphrase.or(env_key.as_deref());

    let mut args = vec!["connect".to_string(), "-n".to_string(), agent.to_string()];
    if let Some(key) = effective_key {
        args.push("-k".to_string());
        args.push(key.to_string());
    }
    args.push("127.0.0.1".to_string());
    args.push(SHELL_PORT.to_string());

    let mut child = Command::new(std::env::current_exe()?)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())  // logs go to stderr, not stdout
        .spawn()?;

    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout_pipe = child.stdout.take().expect("stdout");

    // Send initial terminal size: cols (u16 BE) then rows (u16 BE)
    let (cols, rows) = terminal_size()?;
    stdin.write_all(&cols.to_be_bytes())?;
    stdin.write_all(&rows.to_be_bytes())?;
    stdin.flush()?;

    let enc_label = if effective_key.is_some() { " [E2E encrypted]" } else { "" };
    eprintln!("\x1b[32m[sra] connected to {}{} — Ctrl+] to exit\x1b[0m", agent, enc_label);

    enable_raw_mode()?;

    // Thread: agent PTY output → local stdout
    let output_thread = std::thread::spawn(move || {
        let mut header = [0u8; 1];
        let mut out = std::io::stdout();
        loop {
            if stdout_pipe.read_exact(&mut header).is_err() { break; }
            if header[0] == MSG_DATA {
                let mut len_buf = [0u8; 2];
                if stdout_pipe.read_exact(&mut len_buf).is_err() { break; }
                let len = u16::from_be_bytes(len_buf) as usize;
                let mut data = vec![0u8; len];
                if stdout_pipe.read_exact(&mut data).is_err() { break; }
                if out.write_all(&data).is_err() { break; }
                let _ = out.flush();
            }
        }
    });

    // Main thread: keyboard + resize events → agent PTY
    loop {
        if poll(Duration::from_millis(50))? {
            match read()? {
                Event::Key(key) => {
                    // Ctrl+] exits (telnet-style escape)
                    if key.code == KeyCode::Char(']')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break;
                    }
                    let bytes = key_to_bytes(key.code, key.modifiers);
                    if bytes.is_empty() { continue; }
                    let len = bytes.len() as u16;
                    let mut frame = Vec::with_capacity(3 + bytes.len());
                    frame.push(MSG_DATA);
                    frame.extend_from_slice(&len.to_be_bytes());
                    frame.extend_from_slice(&bytes);
                    stdin.write_all(&frame)?;
                    stdin.flush()?;
                }
                Event::Resize(cols, rows) => {
                    let mut frame = vec![MSG_RESIZE];
                    frame.extend_from_slice(&cols.to_be_bytes());
                    frame.extend_from_slice(&rows.to_be_bytes());
                    stdin.write_all(&frame)?;
                    stdin.flush()?;
                }
                _ => {}
            }
        }
        if output_thread.is_finished() { break; }
    }

    disable_raw_mode()?;
    println!();
    let _ = child.kill();
    let _ = child.wait();
    eprintln!("\x1b[32m[sra] session ended\x1b[0m");
    Ok(())
}

fn key_to_bytes(code: KeyCode, modifiers: KeyModifiers) -> Vec<u8> {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Char(c) => {
            if ctrl {
                let b = c as u8;
                if b.is_ascii_lowercase() { vec![b - b'a' + 1] }
                else if b.is_ascii_uppercase() { vec![b - b'A' + 1] }
                else { c.to_string().into_bytes() }
            } else { c.to_string().into_bytes() }
        }
        KeyCode::Enter     => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Delete    => vec![0x1b, b'[', b'3', b'~'],
        KeyCode::Tab       => vec![b'\t'],
        KeyCode::Esc       => vec![0x1b],
        KeyCode::Up        => vec![0x1b, b'[', b'A'],
        KeyCode::Down      => vec![0x1b, b'[', b'B'],
        KeyCode::Right     => vec![0x1b, b'[', b'C'],
        KeyCode::Left      => vec![0x1b, b'[', b'D'],
        KeyCode::Home      => vec![0x1b, b'[', b'H'],
        KeyCode::End       => vec![0x1b, b'[', b'F'],
        KeyCode::PageUp    => vec![0x1b, b'[', b'5', b'~'],
        KeyCode::PageDown  => vec![0x1b, b'[', b'6', b'~'],
        KeyCode::F(1)      => vec![0x1b, b'O', b'P'],
        KeyCode::F(2)      => vec![0x1b, b'O', b'Q'],
        KeyCode::F(3)      => vec![0x1b, b'O', b'R'],
        KeyCode::F(4)      => vec![0x1b, b'O', b'S'],
        _                  => vec![],
    }
}
```

### 4.7 Wire Shell into `sra-client/src/main.rs`

Add at top:
```rust
mod shell;
```

In the `start()` function, add a Shell check BEFORE the main `loop {}`.
Find where `args.arg_commands` is matched or used and add:
```rust
if let ArgCommands::Shell(shell_args) = &args.arg_commands {
    return shell::run_shell(
        &shell_args.agent_name,
        shell_args.cryptography.as_deref(),
    ).map_err(|e| ClientError::IoError(e.to_string()));
}
```

If `ClientError` doesn't have `IoError`, use the closest available error variant
or add `IoError(String)` to the enum.

---

## Phase 5 — E2E encryption configuration

Already implemented in the codebase. No code changes needed.

### 5.1 Agent config

`/etc/sra/agent.yaml` with E2E:
```yaml
endpoints:
  - !SelfHosted
    gateway: gw.sra.sh:443
    token: "eyJ..."
    e2ee:
      - !PassPhrase
        phrase: "your-strong-passphrase"
        policy: Strict
```

Generate passphrase:
```bash
openssl rand -base64 32
```

### 5.2 Create `docs/e2e-encryption.md`

```markdown
# E2E Encryption

XChaCha20-Poly1305 end-to-end encryption. When enabled, the gateway and
Cloudflare see only ciphertext. Only operator and agent can read the stream.

## Setup

1. openssl rand -base64 32   → save as your passphrase
2. Add to /etc/sra/agent.yaml: e2ee.phrase + policy: Strict
3. systemctl restart sra-agent
4. sra shell -n <agent> -k <passphrase>
   OR: export SRA_E2EE_KEY=<passphrase> && sra shell -n <agent>

## Why SRA_E2EE_KEY env var

Using -k puts the passphrase in shell history (~/.zsh_history).
The env var approach keeps it out of history.

## Cipher details

- Encryption: XChaCha20-Poly1305
- Tamper detection: HMAC-SHA256
- Key derivation: SHA3-256(passphrase XOR nonce) — unique key per connection
```

---

## Phase 6 — GitHub Actions CI/CD

Create `.github/workflows/ci.yml`:

```yaml
name: CI

on:
  push:
    branches: [main]
    tags: ["v*"]
  pull_request:

env:
  CARGO_TERM_COLOR: always

jobs:
  build:
    strategy:
      matrix:
        include:
          - os: ubuntu-latest
            target: x86_64-unknown-linux-musl
            suffix: linux-amd64
          - os: ubuntu-latest
            target: aarch64-unknown-linux-musl
            suffix: linux-arm64
          - os: macos-latest
            target: aarch64-apple-darwin
            suffix: macos-arm64
          - os: macos-latest
            target: x86_64-apple-darwin
            suffix: macos-amd64

    runs-on: ${{ matrix.os }}

    steps:
      - uses: actions/checkout@v4

      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}

      - name: Install musl tools (Linux only)
        if: contains(matrix.target, 'musl')
        run: |
          sudo apt-get update -q
          sudo apt-get install -y musl-tools
          if [ "${{ matrix.target }}" = "aarch64-unknown-linux-musl" ]; then
            sudo apt-get install -y gcc-aarch64-linux-gnu
          fi

      - uses: Swatinem/rust-cache@v2

      - name: Build
        run: cargo build --workspace --release --target ${{ matrix.target }}

      - name: Package binaries
        run: |
          mkdir dist
          for bin in sra-gateway sra-agent sra sra-token; do
            src="target/${{ matrix.target }}/release/${bin}"
            [ -f "$src" ] && cp "$src" "dist/${bin}-${{ matrix.suffix }}"
          done

      - uses: actions/upload-artifact@v4
        with:
          name: sra-${{ matrix.suffix }}
          path: dist/

  release:
    needs: build
    if: startsWith(github.ref, 'refs/tags/v')
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - uses: actions/download-artifact@v4
        with:
          path: artifacts/
          merge-multiple: true
      - uses: softprops/action-gh-release@v2
        with:
          files: artifacts/**
          generate_release_notes: true
```

---

## Phase 7 — Agent installer script

Create `scripts/install-agent.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

# Usage:
#   AGENT_TOKEN="eyJ..." bash install-agent.sh
#   AGENT_TOKEN="eyJ..." SRA_E2EE_KEY="passphrase" bash install-agent.sh
#   curl -sSL https://raw.githubusercontent.com/flaviuvlaicu/sra/main/scripts/install-agent.sh \
#     | AGENT_TOKEN="eyJ..." bash

GATEWAY="${GATEWAY:-gw.sra.sh:443}"
AGENT_TOKEN="${AGENT_TOKEN:?Set AGENT_TOKEN before running}"
VERSION="${VERSION:-latest}"

ARCH=$(uname -m)
case "$ARCH" in
  x86_64)  SUFFIX="linux-amd64" ;;
  aarch64) SUFFIX="linux-arm64" ;;
  *)       echo "Unsupported arch: $ARCH"; exit 1 ;;
esac

if [ "$VERSION" = "latest" ]; then
  VERSION=$(curl -sSf https://api.github.com/repos/flaviuvlaicu/sra/releases/latest \
    | grep tag_name | cut -d'"' -f4)
fi

echo "[sra] Installing sra-agent ${VERSION} for ${ARCH}..."

curl -sSfLo /usr/local/bin/sra-agent \
  "https://github.com/flaviuvlaicu/sra/releases/download/${VERSION}/sra-agent-${SUFFIX}"
chmod 755 /usr/local/bin/sra-agent   # explicit — scp/curl don't preserve execute bit

mkdir -p /etc/sra
chmod 700 /etc/sra

cat > /etc/sra/agent.yaml <<EOF
endpoints:
  - !SelfHosted
    gateway: ${GATEWAY}
    token: "${AGENT_TOKEN}"
EOF

if [ -n "${SRA_E2EE_KEY:-}" ]; then
  cat >> /etc/sra/agent.yaml <<EOF
    e2ee:
      - !PassPhrase
        phrase: "${SRA_E2EE_KEY}"
        policy: Strict
EOF
fi

chmod 600 /etc/sra/agent.yaml   # protect token and passphrase

cat > /etc/systemd/system/sra-agent.service <<'SVCEOF'
[Unit]
Description=SRA Agent
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/sra-agent -c /etc/sra/agent.yaml
Restart=always
RestartSec=10
User=root
StartLimitIntervalSec=0

[Install]
WantedBy=multi-user.target
SVCEOF

systemctl daemon-reload
systemctl enable --now sra-agent
echo "[sra] Agent installed and started"
```

---

## Phase 8 — Deployment configs

### Gateway VPS

Generate secret:
```bash
openssl rand -hex 32 | fold -w2 | awk '{printf "0x%s, ", $1}' | sed 's/, $//'
```

`/etc/sra/gateway.yaml`:
```yaml
name: sra-gateway
secret: [0x59, 0x1b, ...]   # paste byte array from command above

services:
  - !Ws
    domains: ["gw.sra.sh"]
    listen_addr: "127.0.0.1:80"   # localhost only — cloudflared is public side
```

```bash
chmod 600 /etc/sra/gateway.yaml
```

`/etc/systemd/system/sra-gateway.service`:
```ini
[Unit]
Description=SRA Gateway
After=network.target

[Service]
ExecStart=/usr/local/bin/sra-gateway -c /etc/sra/gateway.yaml
Restart=always
RestartSec=5
User=root

[Install]
WantedBy=multi-user.target
```

### Cloudflare Tunnel

```bash
cloudflared tunnel login
cloudflared tunnel create sra-gateway
# Note the tunnel UUID from the output
```

`/etc/cloudflared/config.yml`:
```yaml
tunnel: <tunnel-uuid>
credentials-file: /etc/cloudflared/<tunnel-uuid>.json

ingress:
  - hostname: gw.sra.sh
    service: http://localhost:80
    originRequest:
      noTLSVerify: true
      disableChunkedEncoding: true   # prevents 502 on WebSocket upgrade
  - service: http_status:404
```

Delete any existing A record for `gw.sra.sh` in Cloudflare DNS, then:
```bash
cloudflared tunnel route dns sra-gateway gw.sra.sh
```

```bash
cloudflared service install
# Copy credential file to /etc/cloudflared/ and update credentials-file path
cp ~/.cloudflared/<uuid>.json /etc/cloudflared/<uuid>.json
systemctl enable --now cloudflared
```

Cloudflare dashboard settings:
- SSL/TLS → Overview → **Flexible**
- Network → WebSockets → **On**

### Token generator

`/etc/sra/token-gen.yaml`:
```yaml
secret: [0x59, 0x1b, ...]   # same as gateway.yaml

tokens:
  - !Agent
    uid: "00000000-0000-0000-0000-000000000001"
    name: sramon
    exp: 1893456000   # 2030-01-01 — set a calendar reminder to regenerate

  - !Client
    uid: "00000000-0000-0000-0000-000000000001"
    name: Nostrom
    exp: 1893456000
```

```bash
chmod 600 /etc/sra/token-gen.yaml
sra-token -c /etc/sra/token-gen.yaml
```

---

## Phase 9 — Build verification checklist

```bash
# 1. Full workspace compiles
cargo build --workspace --release

# 2. No leftover narrowlink references
grep -rn "narrowlink" --include="*.rs" --include="*.toml" . \
  | grep -v target/ | grep -v ".git/"

# 3. Token generator works without panic
./target/release/sra-token -c /etc/sra/token-gen.yaml

# 4. Agent binary is executable
ls -la ./target/release/sra-agent

# 5. Shell subcommand exists
./target/release/sra shell --help

# 6. Shell -n flag works
./target/release/sra shell -n testhost --help

# 7. With gateway + agent running:
./target/release/sra list                            # agent appears
./target/release/sra shell -n sramon                 # connects without E2E
./target/release/sra shell -n sramon -k "passphrase" # connects with E2E
SRA_E2EE_KEY="passphrase" ./target/release/sra shell -n sramon  # via env var
```

---

## Known issues from real deployment — read before starting

Every item here was hit in production during the first deployment.

| # | Symptom | Root cause | Fix |
|---|---|---|---|
| 1 | `Could not automatically determine CryptoProvider` panic | jsonwebtoken v10 needs rust_crypto feature | Add `features = ["rust_crypto"]` to jsonwebtoken in every Cargo.toml |
| 2 | `Permission denied` spawning sra-agent in systemd | Binary missing execute bit | `chmod 755 /usr/local/bin/sra-agent` |
| 3 | `Special user nobody` warning then exec failure | nobody user restricted on some distros | Use `User=root` in service file |
| 4 | Cloudflare 521 | A record is DNS-only (grey cloud) | Toggle orange cloud in Cloudflare DNS |
| 5 | Cloudflare 521 even with orange cloud | VPS provider network firewall blocks 80/443 regardless of UFW | Use Cloudflare Tunnel — gateway only needs localhost:80 |
| 6 | Cloudflare 520 | WebSockets disabled in Cloudflare | Dashboard → Network → WebSockets → On |
| 7 | ACME cert never issues | Cloudflare intercepts HTTP-01 challenge | Use `!Ws` + Cloudflare Tunnel; do NOT use `!Wss` with ACME behind Cloudflare |
| 8 | `sra-token: command not found` | Sed produces `sra-token-generator` not `sra-token` | Explicitly set `[[bin]] name = "sra-token"` |
| 9 | cloudflared fails `error parsing tunnel ID: <your-tunnel-id>` | Config has placeholder not real UUID | Replace `<your-tunnel-id>` with actual UUID from `cloudflared tunnel create` output |
| 10 | cloudflared fails with wrong config path | `cloudflared service install` expects `/etc/cloudflared/` not `~/.cloudflared/` | Copy files to `/etc/cloudflared/` and update credentials-file path |
| 11 | `Failed to add route: record already exists` | A record exists when cloudflared tries to create CNAME | Delete the A record in Cloudflare DNS first |
| 12 | Agent connects then gives up after ~2 min | Upstream Narrowlink bug: exit after 70s retries | Phase 2 fix: `sleep_time = (sleep_time + 10).min(60)`, never break on network errors |
| 13 | Cloudflare SSL errors after tunnel is up | SSL mode set to Full Strict but origin is plain HTTP | Set SSL/TLS → Overview → Flexible |
| 14 | macOS `sed -i` fails | GNU sed vs BSD sed incompatibility | Use `perl -pi -e` (works on both) |

---

## Security posture

| Layer | Mechanism | Status |
|---|---|---|
| Token auth | JWT HMAC-SHA256, per-agent | ✓ existing |
| Transport encryption | TLS via Cloudflare edge | ✓ existing |
| E2E encryption | XChaCha20-Poly1305 + HMAC-SHA256 | ✓ wired into `sra shell` via `-k` / `SRA_E2EE_KEY` |
| PTY isolation | `localhost:22222` only — unreachable from network | ✓ shell.rs |
| VPS IP hidden | Cloudflare Tunnel — zero inbound ports on VPS | ✓ cloudflared |
| Config protection | `chmod 600` on all secret-containing files | ✓ install script |
| mTLS per-agent certs | X.509 root CA + leaf certs | ✗ future work |

---

## Final binary names

| Binary | Purpose | Installed on |
|---|---|---|
| `sra-gateway` | Relay server | Gateway VPS |
| `sra-agent` | Outbound agent + PTY server | Every target server |
| `sra` | Operator CLI | Nostrom (operator) |
| `sra-token` | Token generator | Gateway VPS |

## Final command reference

```bash
sra list                                   # list connected agents
sra shell -n sramon                        # shell, TLS only
sra shell -n sramon -k "passphrase"        # shell, E2E encrypted (recommended)
export SRA_E2EE_KEY="passphrase"           # set once, reuse
sra shell -n sramon                        # shell via env var
sra connect -n sramon 127.0.0.1 22222      # raw tunnel to any port
sra-token -c /etc/sra/token-gen.yaml       # generate/regenerate tokens
```
