# AX Codex agent image (DeepSeek)

Builds an AX task image that runs the Codex CLI against an OpenAI-compatible
provider (DeepSeek) on a pre-mounted workspace.

## Contents

| File | Purpose |
| --- | --- |
| `Dockerfile` | Starts from the patched `ax-task-runner` image, adds the Node runtime, the Codex CLI, a provider config, and `run-agent.sh`. |
| `run-agent.sh` | Pins the provider hostname from `base_url` into `/etc/hosts`, then runs `codex exec` in `/workspace` and writes `codex.jsonl`, `codex.err`, `AGENT_DONE`. |
| `config.toml` | DeepSeek provider: `https://api.deepseek.com/v1`, `wire_api = "responses"`, model `deepseek-v4-flash`. |

## Why the hosts pin

Inside the AX/Agent Substrate sandbox, curl and Node could resolve
`api.deepseek.com`, but the Codex HTTP client kept failing with
`Connection failed: error sending request`. Pinning the resolved IPv4 address
from `base_url` into `/etc/hosts` before starting Codex fixed it. The script
does this at container start, so it also works when the address changes between
snapshots.

## Build

The Codex CLI is copied from the local install rather than installed from npm,
so the image is reproducible offline:

```bash
cp -aL ~/.local/share/myaiim-codex-test/node ./node
cp -a  ~/.local/share/myaiim-codex-test/cli  ./cli
docker build -t localhost:5001/codex-agent:latest .
docker push localhost:5001/codex-agent:latest
```

The build context must not commit `node/` or `cli/` (hundreds of MiB).

## Task shape

```yaml
spec:
  image: localhost:5001/codex-agent@sha256:...   # digest-pinned
  command: ["/bin/sh", "-ec", "/usr/local/bin/run-agent.sh; sleep 600"]
  env:
    - { name: AX_PREMOUNTED_WORKSPACE, value: "1" }
    - { name: DEEPSEEK_API_KEY, value: "<from a Secret, never inline in git>" }
    - { name: AGENT_PROMPT, value: "Create /workspace/hello.txt ..." }
  workspace: { name: agent-ws, path: /workspace }
  debug: true
```

## Verified

- `AX_PREMOUNTED_WORKSPACE=1` uses the mounted workspace without git setup;
  a DeepSeek run completed with `turn.completed` (`input_tokens=28118`,
  `output_tokens=1724`).
- The egress gateway recorded TLS-passthrough tunnels to `api.deepseek.com:443`
  with upstream code 200 and no failures.
- `/workspace/hello.txt` matched the expected bytes and SHA-256; `SUMMARY.md`
  listed the workspace.
