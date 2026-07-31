# One Dockerfile for all four services.
#
# They share a Cargo workspace, so building them from one file keeps the
# dependency graph and toolchain in a single place. `SERVICE` picks which
# binary ends up in the runtime image:
#
#   docker build -f Dockerfile --build-arg SERVICE=auth -t <image> .
#
# The builder stage deliberately takes no build args and builds the *whole*
# workspace, so it is byte-identical across all four services. That is what lets
# BuildKit compile the dependency tree once and reuse it for the other three
# images -- building per-service instead (`cargo build -p perfice-auth`) makes
# each image a distinct cache entry and repeats the entire build four times,
# which is both slow and enough concurrent memory to get cargo OOM-killed.
#
# Build context is `server/`, because the proto crate compiles
# `server/proto/auth.proto` at build time.

FROM rust:1-bookworm AS builder

# protobuf-compiler is used by the proto crate's build script.
RUN apt-get update \
    && apt-get install -y --no-install-recommends protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY rust/ ./rust/
COPY proto/auth.proto ./proto/auth.proto

WORKDIR /build/rust
# --locked so a build never silently resolves different dependency versions
# than the ones the test suite ran against.
RUN cargo build --release --locked

FROM debian:bookworm-slim

# ca-certificates for outbound TLS to providers and mail, tzdata because every
# integration schedule is evaluated in the user's timezone.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --uid 10001 perfice

ARG SERVICE
RUN test -n "$SERVICE" || (echo "SERVICE build-arg is required" >&2 && exit 1)

COPY --from=builder /build/rust/target/release/perfice-${SERVICE} /usr/local/bin/perfice

USER perfice
WORKDIR /home/perfice

# Ports are set by the service's environment; see docker-compose.yml.
CMD ["perfice"]
