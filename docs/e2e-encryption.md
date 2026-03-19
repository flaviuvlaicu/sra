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
