#!/usr/bin/env sh
# Pushes the service images built by build.sh.
#
#   ./push.sh            # all four
#   ./push.sh auth sync  # just these
set -eu

REGISTRY="${REGISTRY:-ghcr.io/p0lloc}"
TAG="${TAG:-latest}"

services="${*:-auth sync gateway integration}"

for service in $services; do
    docker push "$REGISTRY/perfice_$service:$TAG"
done
