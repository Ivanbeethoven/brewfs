#!/usr/bin/env bash
# Install Go and ko, and check the tools this deployment needs.
set -euo pipefail
source "$(dirname "$0")/env.sh"

need() { command -v "$1" >/dev/null 2>&1; }

if ! need curl; then
  ${SUDO} apt-get update -qq
  ${SUDO} apt-get install -y -qq curl ca-certificates >/dev/null
fi

if ! need go; then
  echo "==> installing Go ${GO_VERSION}"
  tmp="$(mktemp -d)"
  if curl -4 -fsSL -m 300 "https://dl.google.com/go/go${GO_VERSION}.linux-amd64.tar.gz" -o "${tmp}/go.tgz"; then
    :
  else
    fetch_github "https://go.dev/dl/go${GO_VERSION}.linux-amd64.tar.gz" "${tmp}/go.tgz"
  fi
  rm -rf "${HOME}/.local/go"
  tar -C "${HOME}/.local" -xzf "${tmp}/go.tgz"
  rm -rf "${tmp}"
fi
go version

if ! need ko; then
  echo "==> installing ko ${KO_VERSION}"
  GOBIN="${LOCAL_BIN}" go install "github.com/google/ko@${KO_VERSION}"
fi
ko version

if ! need docker; then
  echo "WARNING: docker is not installed."
  echo "         k3s does not need it, but 05-ax.sh builds the task-runner image with it."
fi

echo "prerequisites OK"
