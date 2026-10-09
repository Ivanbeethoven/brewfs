FROM rust:1.96-bookworm AS builder

WORKDIR /workspace
RUN apt-get update \
    && apt-get install -y --no-install-recommends binutils protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

COPY . .
WORKDIR /workspace/operator/brewfs-operator
RUN cargo build --release --features workspace-operator \
    && strip target/release/brewfs-operator

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /workspace/operator/brewfs-operator/target/release/brewfs-operator /usr/local/bin/brewfs-operator
ENTRYPOINT ["/usr/local/bin/brewfs-operator"]
