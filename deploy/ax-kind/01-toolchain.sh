#!/usr/bin/env bash
# Install Go, kubectl, kind and ko under ~/.local (no system Go packages).
set -euo pipefail
source "$(dirname "$0")/env.sh"

need() { command -v "$1" >/dev/null 2>&1; }

fetch() { # fetch <url> <dest>
  if [ -n "${AX_DEPLOY_PROXY}" ]; then
    curl -4 -fsSL -x "${AX_DEPLOY_PROXY}" "$1" -o "$2"
  else
    curl -4 -fsSL "$1" -o "$2"
  fi
}

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
  url="https://go.dev/dl/go${GO_VERSION}.linux-amd64.tar.gz"
  if [ -n "${AX_DEPLOY_PROXY}" ]; then
    fetch "${url}" "${tmp}/go.tgz"
  else
    fetch "${GO_DOWNLOAD_BASE}/go${GO_VERSION}.linux-amd64.tar.gz" "${tmp}/go.tgz" ||
      fetch "${url}" "${tmp}/go.tgz"
  fi
  rm -rf "${HOME}/.local/go"
  tar -C "${HOME}/.local" -xzf "${tmp}/go.tgz"
  rm -rf "${tmp}"
fi
go version

if ! need kubectl; then
  echo "==> installing kubectl ${KUBECTL_VERSION}"
  base="https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/amd64/kubectl"
  if [ -n "${AX_DEPLOY_PROXY}" ]; then
    fetch "${base}" "${LOCAL_BIN}/kubectl"
  else
    fetch "https://cdn.dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/amd64/kubectl" "${LOCAL_BIN}/kubectl" ||
      fetch "${base}" "${LOCAL_BIN}/kubectl"
  fi
  chmod +x "${LOCAL_BIN}/kubectl"
fi
kubectl version --client

if ! need kind; then
  echo "==> installing kind ${KIND_VERSION}"
  fetch "https://github.com/kubernetes-sigs/kind/releases/download/${KIND_VERSION}/kind-linux-amd64" "${LOCAL_BIN}/kind"
  chmod +x "${LOCAL_BIN}/kind"
fi
kind version

if ! need ko; then
  echo "==> installing ko ${KO_VERSION} (GOPROXY=${GOPROXY})"
  GOBIN="${LOCAL_BIN}" go install "github.com/google/ko@${KO_VERSION}"
fi
ko version
