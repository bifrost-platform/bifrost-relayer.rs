# build stage: where we create binary
FROM rust:1.70 AS builder

RUN apt update && apt install -y make clang pkg-config libssl-dev protobuf-compiler
RUN rustup default stable && \
  rustup update && \
  rustup update nightly && \
  rustup target add wasm32-unknown-unknown --toolchain nightly

WORKDIR /relayer
ENV CARGO_HOME=/relayer/.cargo
COPY . /relayer
RUN cargo build --release

# 2nd stage: where we run cccp-relayer binary
FROM ubuntu:22.04

COPY --from=builder /relayer/target/release/cccp-relayer /usr/local/bin
COPY --from=builder /relayer/configs /configs

RUN /usr/local/bin/cccp-relayer --version

# 8000 for Prometheus exporter
EXPOSE 8000

VOLUME ["/data"]

ENTRYPOINT ["/usr/local/bin/cccp-relayer"]
