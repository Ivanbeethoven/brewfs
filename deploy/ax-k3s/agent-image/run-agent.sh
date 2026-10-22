#!/bin/sh
set -e
export HOME=/root
export CODEX_HOME=/opt/codex-home
cd /workspace

pin_provider_hosts() {
  config="$CODEX_HOME/config.toml"
  if [ ! -r "$config" ]; then
    return 0
  fi

  hosts="$(sed -n 's/^[[:space:]]*base_url[[:space:]]*=[[:space:]]*"https:\/\/\([^\/"]*\).*/\1/p' "$config" | sort -u)"
  for host in $hosts; do
    host="${host%%:*}"
    if [ -z "$host" ]; then
      continue
    fi
    if grep -Eq "(^|[[:space:]])${host}([[:space:]]|$)" /etc/hosts 2>/dev/null; then
      continue
    fi

    if ips="$(timeout 5 node -e '
      const dns = require("node:dns");
      const host = process.argv[1];
      const print = (addresses) => {
        for (const address of addresses) {
          console.log(typeof address === "string" ? address : address.address);
        }
      };
      dns.resolve4(host, (error, addresses) => {
        if (!error && addresses.length > 0) {
          print(addresses);
          return;
        }
        dns.lookup(host, { family: 4, all: true }, (lookupError, lookedUp) => {
          if (lookupError || lookedUp.length === 0) {
            process.exit(1);
          }
          print(lookedUp);
        });
      });
    ' "$host" 2>/dev/null)"; then
      for ip in $ips; do
        printf '%s %s\n' "$ip" "$host" >> /etc/hosts 2>/dev/null || true
      done
    else
      echo "warning: could not resolve provider host $host; continuing" >&2
    fi
  done
}

pin_provider_hosts

PROMPT="${AGENT_PROMPT:-}"
if [ -z "$PROMPT" ]; then
  if [ -f TASK.md ]; then
    PROMPT="$(cat TASK.md)"
  else
    PROMPT="Inspect /workspace and write a short SUMMARY.md describing what you found."
  fi
fi
codex --disable plugins --disable remote_plugin exec --json --skip-git-repo-check --sandbox danger-full-access "$PROMPT" </dev/null >/workspace/codex.jsonl 2>/workspace/codex.err || true
echo done >/workspace/AGENT_DONE
