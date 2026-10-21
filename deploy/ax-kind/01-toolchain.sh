#!/usr/bin/env bash
# Install Go, kubectl, kind and ko under ~/.local (no system Go packages).
set -euo pipefail
source "$(dirname "$0")/env.sh"

need() { command -v "$1" >/dev/null 2>&1; }

if ! need make; then
  if [ "$(id -u)" -eq 0 ]; then
    apt-get update -qq && apt-get install -y -qq make ca-certificates curl >/dev/null
  else
    sudo apt-get update -qq && sudo apt-get install -y -qq make ca-certificates curl >/dev/null
  fi
fi

if ! need go; then
  echo "==> installing Go ${GO_VERSION}"
  tmp="$(mktemp -d)"
  curl -fsSL "https://go.dev/dl/go${GO_VERSION}.linux-amd64.tar.gz" -o "${tmp}/go.tgz"
  rm -rf "${HOME}/.local/go"
  tar -C "${HOME}/.local" -xzf "${tmp}/go.tgz"
  rm -rf "${tmp}"
fi
go version

if ! need kubectl; then
  echo "==> installing kubectl ${KUBECTL_VERSION}"
  curl -fsSL "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/amd64/kubectl" -o "${LOCAL_BIN}/kubectl"
  chmod +x "${LOCAL_BIN}/kubectl"
fi
kubectl version --client

if ! need kind; then
  echo "==> installing kind ${KIND_VERSION}"
  curl -fsSL "https://kind.sigs.k8s.io/dl/${KIND_VERSION}/kind-linux-amd64" -o "${LOCAL_BIN}/kind"
  chmod +x "${LOCAL_BIN}/kind"
fi
kind version

if ! need ko; then
  echo "==> installing ko ${KO_VERSION}"
  GOBIN="${LOCAL_BIN}" go install "github.com/google/ko@${KO_VERSION}"
fi
ko version
