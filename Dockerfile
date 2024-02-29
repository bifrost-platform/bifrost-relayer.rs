# build stage: where we create binary
FROM rust:latest AS builder

RUN apt update && apt install -y protobuf-compiler

WORKDIR /relayer
COPY . /relayer

RUN cargo build --release

# 2nd stage: where we run bifrost-relayer binary
FROM debian:stable-slim

COPY --from=builder /relayer/target/release/bifrost-relayer /usr/local/bin
COPY --from=builder /relayer/configs /configs

RUN /usr/local/bin/bifrost-relayer --version

# 8000 for Prometheus exporter
EXPOSE 8000

VOLUME ["/data"]

ENTRYPOINT ["/usr/local/bin/bifrost-relayer"]
