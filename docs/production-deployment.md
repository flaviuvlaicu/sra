# SRA Production Deployment Guide

This document walks through deploying SRA end-to-end: gateway on a VPS behind Cloudflare Tunnel, agents on target servers, and the operator CLI on your local machine.

## Prerequisites

- A VPS (any provider — the IP will be hidden behind Cloudflare Tunnel)
- A domain managed by Cloudflare DNS (e.g. `sra.sh` with `gw.sra.sh` for the gateway)
- `cloudflared` installed on the VPS
- Rust toolchain on your build machine (or use GitHub Actions releases)

---

## 1. Build the binaries

### Option A: GitHub Actions (recommended)

Tag a release to trigger the CI pipeline:

```bash
git tag v0.1.0
git push origin v0.1.0
```

This builds static binaries for:
- `linux-amd64` (x86_64-unknown-linux-musl)
- `linux-arm64` (aarch64-unknown-linux-musl)
- `macos-arm64` (aarch64-apple-darwin)
- `macos-amd64` (x86_64-apple-darwin)

Download from the GitHub Releases page.

### Option B: Local build

```bash
cargo build --workspace --release
ls target/release/sra-gateway target/release/sra-agent target/release/sra target/release/sra-token
```

---

## 2. Set up the gateway VPS

### 2.1 Generate the shared secret

The secret is a byte array shared between the gateway and the token generator. It must be at least 8 bytes.

```bash
openssl rand -hex 32 | fold -w2 | awk '{printf "0x%s, ", $1}' | sed 's/, $/\n/'
```

Example output: `0x59, 0x1b, 0xa3, 0xf7, ...` (32 bytes = 64 hex chars)

Save this — you'll use it in both `gateway.yaml` and `token-gen.yaml`.

### 2.2 Install the gateway binary

```bash
scp target/release/sra-gateway root@<vps>:/usr/local/bin/
ssh root@<vps> chmod 755 /usr/local/bin/sra-gateway
```

### 2.3 Create the gateway config

```bash
ssh root@<vps>
mkdir -p /etc/sra
```

Create `/etc/sra/gateway.yaml`:

```yaml
name: sra-gateway
secret: [0x59, 0x1b, 0xa3, 0xf7, ...]   # paste your byte array here

services:
  - !Ws
    domains: ["gw.sra.sh"]
    listen_addr: "127.0.0.1:80"
```

Key points:
- Use `!Ws` (plain WebSocket), NOT `!Wss` — Cloudflare handles TLS termination
- Bind to `127.0.0.1:80` — only cloudflared connects locally, nothing is exposed to the internet
- Do NOT use ACME certificates — Cloudflare intercepts the HTTP-01 challenge and it will never issue

Lock down permissions:

```bash
chmod 600 /etc/sra/gateway.yaml
```

### 2.4 Create the systemd service

Create `/etc/systemd/system/sra-gateway.service`:

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

```bash
systemctl daemon-reload
systemctl enable --now sra-gateway
journalctl -u sra-gateway -f   # verify it starts
```

---

## 3. Set up Cloudflare Tunnel

This is what hides your VPS IP and provides TLS to clients.

### 3.1 Install cloudflared and authenticate

```bash
# On the VPS
curl -sSfL https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64 \
  -o /usr/local/bin/cloudflared
chmod 755 /usr/local/bin/cloudflared
cloudflared tunnel login
```

### 3.2 Create the tunnel

```bash
cloudflared tunnel create sra-gateway
```

Note the tunnel UUID from the output (e.g. `a1b2c3d4-...`).

### 3.3 Configure the tunnel

Copy credentials to the system path:

```bash
mkdir -p /etc/cloudflared
cp ~/.cloudflared/<tunnel-uuid>.json /etc/cloudflared/<tunnel-uuid>.json
```

Create `/etc/cloudflared/config.yml`:

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

### 3.4 Set up DNS

Delete any existing A record for `gw.sra.sh` in Cloudflare DNS first, then:

```bash
cloudflared tunnel route dns sra-gateway gw.sra.sh
```

This creates a CNAME record pointing to the tunnel.

### 3.5 Cloudflare dashboard settings

Go to the Cloudflare dashboard for your domain:

1. **SSL/TLS > Overview** — set to **Flexible** (gateway listens on plain HTTP; Cloudflare handles TLS)
2. **Network > WebSockets** — set to **On**

### 3.6 Start cloudflared as a service

```bash
cloudflared service install
systemctl enable --now cloudflared
```

### 3.7 Verify the gateway is reachable

From your local machine:

```bash
curl -s -o /dev/null -w "%{http_code}" https://gw.sra.sh/
```

You should get `200` (the gateway's welcome page).

---

## 4. Generate tokens

### 4.1 Install sra-token on the VPS

```bash
scp target/release/sra-token root@<vps>:/usr/local/bin/
ssh root@<vps> chmod 755 /usr/local/bin/sra-token
```

### 4.2 Create the token config

Create `/etc/sra/token-gen.yaml` on the VPS:

```yaml
secret: [0x59, 0x1b, 0xa3, 0xf7, ...]   # MUST match gateway.yaml

tokens:
  - !Agent
    uid: "00000000-0000-0000-0000-000000000001"
    name: sramon
    exp: 1893456000       # 2030-01-01 — set a calendar reminder

  - !Client
    uid: "00000000-0000-0000-0000-000000000002"
    name: Nostrom
    exp: 1893456000
```

For each additional agent, add another `!Agent` block with a unique `uid` and `name`. Same for additional operators with `!Client`.

```bash
chmod 600 /etc/sra/token-gen.yaml
```

### 4.3 Generate the tokens

```bash
sra-token -c /etc/sra/token-gen.yaml
```

This prints JWT tokens to stdout — one per entry. Save them:

- **Agent token** (`sramon`): Goes into the agent's `agent.yaml`
- **Client token** (`Nostrom`): Goes into the operator's `client.yaml`

---

## 5. Deploy agents

### Option A: Automated installer (recommended)

From the target server (or via SSH):

```bash
curl -sSL https://raw.githubusercontent.com/flaviuvlaicu/sra/main/scripts/install-agent.sh \
  | AGENT_TOKEN="eyJ..." SRA_E2EE_KEY="your-passphrase" bash
```

Or with a specific gateway address:

```bash
GATEWAY="gw.sra.sh:443" \
AGENT_TOKEN="eyJ..." \
SRA_E2EE_KEY="your-passphrase" \
  bash scripts/install-agent.sh
```

This:
1. Downloads the correct binary for the architecture
2. Creates `/etc/sra/agent.yaml` with the token and E2E config
3. Creates and starts the systemd service

### Option B: Manual install

Copy the binary:

```bash
scp target/release/sra-agent root@<server>:/usr/local/bin/
ssh root@<server> chmod 755 /usr/local/bin/sra-agent
```

Create `/etc/sra/agent.yaml`:

```yaml
endpoints:
  - !SelfHosted
    gateway: gw.sra.sh:443
    token: "eyJ..."
e2ee:
  - !PassPhrase
    phrase: "your-passphrase"
    policy: Strict
```

- `policy: Strict` — rejects connections that don't provide the passphrase
- `policy: Lax` — allows both encrypted and unencrypted connections

```bash
chmod 600 /etc/sra/agent.yaml
```

Create `/etc/systemd/system/sra-agent.service`:

```ini
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
```

```bash
systemctl daemon-reload
systemctl enable --now sra-agent
journalctl -u sra-agent -f   # verify it connects
```

You should see: `Connection successful`

---

## 6. Configure the operator machine

### 6.1 Install the sra binary

Copy or download `sra` (the client binary) to your local machine:

```bash
# From a release
curl -sSfLo /usr/local/bin/sra \
  "https://github.com/flaviuvlaicu/sra/releases/download/v0.1.0/sra-macos-arm64"
chmod 755 /usr/local/bin/sra
```

### 6.2 Create the client config

Create `~/.sra/client.yaml`:

```yaml
gateways:
  - !SelfHosted
    gateway: gw.sra.sh:443
    token: "eyJ..."                # client token from sra-token
```

Alternatively use `/etc/sra/client.yaml` on Linux.

### 6.3 Generate the E2E passphrase

```bash
openssl rand -base64 32
```

Save this passphrase. It must match the `phrase` field in each agent's `agent.yaml`. You can either:

- Pass it each time: `sra shell -n sramon -k "your-passphrase"`
- Set it once per session: `export SRA_E2EE_KEY="your-passphrase"` (avoids shell history)

---

## 7. Verify the deployment

### 7.1 List connected agents

```bash
sra list
```

You should see your agent(s) listed.

### 7.2 Open a shell

```bash
# TLS only (gateway can see traffic)
sra shell -n sramon

# E2E encrypted (recommended)
sra shell -n sramon -k "your-passphrase"

# Or via env var
export SRA_E2EE_KEY="your-passphrase"
sra shell -n sramon
```

You should get a bash prompt on the remote server. Press `Ctrl+]` to exit.

### 7.3 Test port forwarding

```bash
# Forward local port 8080 to agent's localhost:80
sra forward -n sramon -l 127.0.0.1:8080 127.0.0.1:80
```

---

## 8. Adding more agents

For each new server:

1. Add an `!Agent` entry in `token-gen.yaml` with a unique `uid` and `name`
2. Regenerate tokens: `sra-token -c /etc/sra/token-gen.yaml`
3. Deploy the agent with the new token (use the installer script or manual method)

The E2E passphrase can be the same across all agents or different per agent (you just need to pass the right one when connecting).

---

## 9. Operations

### Viewing agent logs

```bash
journalctl -u sra-agent -f
```

### Viewing gateway logs

```bash
journalctl -u sra-gateway -f
```

### Restarting after config changes

```bash
systemctl restart sra-agent
systemctl restart sra-gateway
```

### Rotating tokens

1. Edit `token-gen.yaml` with new expiration dates
2. Run `sra-token -c /etc/sra/token-gen.yaml`
3. Update each agent's `agent.yaml` with the new token
4. Update each operator's `client.yaml` with the new token
5. Restart agents: `systemctl restart sra-agent`

### Upgrading binaries

1. Tag a new release or build locally
2. Copy new binaries to each machine
3. Restart services

---

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| Agent logs: `Authentication failed — check your token` | Bad or expired JWT token | Regenerate with `sra-token` and redeploy |
| Agent logs: retries then connects | Normal — agent retries every 10-60s | Wait for connection |
| `sra list` returns empty | Agent not connected, or wrong client token | Check agent logs, verify tokens share same secret |
| Cloudflare 521 error | Gateway not running or cloudflared misconfigured | Check `systemctl status sra-gateway` and `systemctl status cloudflared` |
| Cloudflare 520 error | WebSockets not enabled in Cloudflare | Dashboard > Network > WebSockets > On |
| `sra shell` connects but no prompt | E2E passphrase mismatch | Verify `-k` value matches agent's `agent.yaml` phrase |
| `Could not automatically determine CryptoProvider` | Missing `rust_crypto` feature on jsonwebtoken | Already fixed in this fork — rebuild from latest source |
| `Permission denied` starting agent | Binary missing execute bit | `chmod 755 /usr/local/bin/sra-agent` |
| SSL errors through Cloudflare | SSL mode set to Full/Strict | Set SSL/TLS > Overview > **Flexible** |
| `Failed to add route: record already exists` | DNS A record conflicts with tunnel CNAME | Delete the A record in Cloudflare DNS, then re-run `cloudflared tunnel route dns` |
| Agent exits after ~2 minutes | Running old Narrowlink code without the retry fix | Rebuild from SRA source (the fix caps backoff at 60s, never exits) |

---

## Security checklist

- [ ] Gateway secret is random, at least 32 bytes: `openssl rand -hex 32`
- [ ] All config files containing secrets are `chmod 600`
- [ ] `/etc/sra/` directory is `chmod 700`
- [ ] E2E encryption enabled with `policy: Strict` on all agents
- [ ] E2E passphrase is passed via `SRA_E2EE_KEY` env var (not `-k` in shell history)
- [ ] Token expiration dates are set and calendar reminders created
- [ ] VPS has no public-facing ports (only cloudflared connects locally)
- [ ] Cloudflare SSL mode is Flexible (not Full/Strict)
- [ ] WebSockets enabled in Cloudflare dashboard
- [ ] No DNS A record for gateway hostname (only tunnel CNAME)

---

## Architecture summary

```
Operator                 Cloudflare              VPS                    Target Server
────────                 ──────────              ───                    ─────────────
sra shell ──WSS──────► gw.sra.sh:443 ─tunnel─► sra-gateway ◄──WSS── sra-agent
  │                     (TLS termination)        (localhost:80)        (outbound only)
  │                                                                        │
  └─ XChaCha20-Poly1305 ─────────────────────────────────────────────────►│
     (E2E: gateway sees only ciphertext)                              PTY shell
                                                                    (localhost:22222)
```
