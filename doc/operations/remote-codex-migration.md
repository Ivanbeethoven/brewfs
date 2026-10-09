# Remote Codex migration

This repository keeps remote Codex setup separate from ECS lifecycle and test
runners. The helper at
`docker/compose-xfstests/aliyun/remote_codex_migrate.ps1` assumes that an SSH
host already exists and only performs a non-destructive repository check plus
credential configuration.

## What the helper does

The helper:

1. Verifies passwordless SSH and checks the remote Git installation.
2. Clones the requested public GitHub branch into `/opt/brewfs` when the path
   does not exist. An existing Git checkout is only inspected; it is never
   reset, cleaned, or force-checked-out.
3. Sends the local GitHub CLI token over SSH stdin to `gh auth login --with-token`.
4. Sends the local Aliyun CLI config over SSH stdin and writes it as
   `~/.aliyun/config.json` with mode `0600`.
5. Extracts only the custom provider URL and bearer token from the local Codex
   config, writes a minimal Linux config as `~/.codex/config.toml`, and grants
   trust to the requested repository.

Tokens and the Aliyun secret are never command-line arguments, written to the
repository, or printed by the helper. The remote shell still needs `gh` and
the Aliyun CLI installed before enabling those transfer steps.

## Example

Create and verify the host and SSH alias separately, then run:

```powershell
pwsh -File .\docker\compose-xfstests\aliyun\remote_codex_migrate.ps1 `
  -SshHost brewfs-aliyun-dev `
  -RemoteRepo /opt/brewfs `
  -Repository https://github.com/Ivanbeethoven/brewfs.git `
  -Branch codex/packed-metadata-aliyun-20260930
```

Use `-SkipGitHub`, `-SkipAliyun`, or `-SkipCodex` when a credential must not be
copied. The script does not start Codex, run benchmarks, create ECS resources,
or delete resources; those actions remain explicit operator steps.

## Cleanup

After the remote work is complete, stop benchmark processes and remove the ECS
instance, disks, public IPs, and temporary key pair using the exact resource
IDs recorded during provisioning. Remove the SSH alias and host-key entry from
the local SSH configuration after the instance is gone. Do not delete unrelated
ECS, OSS, Redis, or security-group resources.
