# The CSI adapter and the BrewFS mount client are shipped together because
# NodePublishVolume starts the existing BrewFS binary in the node plugin
# namespace. Pin BREWFS_IMAGE to the exact BrewFS image used by the cluster.
ARG BREWFS_IMAGE=ghcr.io/ivanbeethoven/brewfs:latest

FROM ${BREWFS_IMAGE} AS brewfs

FROM rust:1.90-bookworm AS builder
WORKDIR /src
RUN apt-get update \
    && apt-get install --no-install-recommends -y protobuf-compiler libprotobuf-dev \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock build.rs ./
COPY proto ./proto
COPY src ./src
RUN cargo build --release --locked

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates mount util-linux \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/brewfs-csi /usr/local/bin/brewfs-csi
COPY --from=brewfs /usr/local/bin/brewfs /usr/local/bin/brewfs
ENTRYPOINT ["/usr/local/bin/brewfs-csi"]
