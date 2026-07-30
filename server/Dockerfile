# One Dockerfile for all four services.
#
# They share a Cargo workspace, so building them from one file keeps the
# dependency graph and toolchain in a single place. `SERVICE` picks which
# binary ends up in the runtime image.
#
# Build context is `server/`, because the proto crate compiles
# `server/proto/auth.proto` at build time:
#
#   docker build -f Dockerfile --build-arg SERVICE=auth -t <image> .

FROM rust:1-bookworm AS builder

# cmake builds the vendored librdkafka; protobuf-compiler is used by the proto
# crate's build script.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY rust/ ./rust/
COPY proto/auth.proto ./proto/auth.proto

ARG SERVICE
RUN test -n "$SERVICE" || (echo "SERVICE build-arg is required" >&2 && exit 1)

WORKDIR /build/rust
RUN cargo build --release --locked -p "perfice-${SERVICE}" \
    && cp "target/release/perfice-${SERVICE}" /usr/local/bin/perfice

FROM debian:bookworm-slim

# ca-certificates for outbound TLS to providers and mail, tzdata because every
# integration schedule is evaluated in the user's timezone.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --uid 10001 perfice

COPY --from=builder /usr/local/bin/perfice /usr/local/bin/perfice

USER perfice
WORKDIR /home/perfice

# Ports are set by the service's environment; see docker-compose.yml.
CMD ["perfice"]
