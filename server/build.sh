#!/usr/bin/env sh
# Builds the service images.
#
#   ./build.sh            # all four
#   ./build.sh auth sync  # just these
#
# Image names are unchanged from the Go implementation, so docker-compose.yml
# and any existing deployment keep working.
set -eu

REGISTRY="${REGISTRY:-ghcr.io/p0lloc}"
TAG="${TAG:-latest}"

services="${*:-auth sync gateway integration}"

for service in $services; do
    echo "==> building $service"
    docker build \
        -f Dockerfile \
        --build-arg "SERVICE=$service" \
        -t "$REGISTRY/perfice_$service:$TAG" \
        .
done
