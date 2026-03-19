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
