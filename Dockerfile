# build stage: where we create binary
FROM rust:latest AS builder

RUN apt update && apt install -y protobuf-compiler

WORKDIR /relayer
COPY . /relayer

RUN cargo build --release

# 2nd stage: where we run bifrost-relayer binary
FROM debian:stable-slim

# TLS clients (rustls/native-roots) read the system CA trust store, which the
# slim image does not ship. Without it: "No CA certificates were loaded from the system".
RUN apt-get update \
	&& apt-get install -y --no-install-recommends ca-certificates \
	&& rm -rf /var/lib/apt/lists/*

COPY --from=builder /relayer/target/release/bifrost-relayer /usr/local/bin
COPY --from=builder /relayer/configs /configs

RUN /usr/local/bin/bifrost-relayer --version

# 8000 for Prometheus exporter
EXPOSE 8000

VOLUME ["/data"]

ENTRYPOINT ["/usr/local/bin/bifrost-relayer"]
