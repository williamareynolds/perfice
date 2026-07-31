# server

Perfice can be run completely locally without a server, but using a server allows some extra features.  
The server component is responsible for synchronizing data between devices and pulling data automatically from integrations like Fitbit, Todoist into the platform.

More information can be read in the [docs](https://perfice.adoe.dev/docs/).

## Layout

```
rust/               the four services (auth, gateway, sync, integration)
proto/auth.proto    the gRPC contract between them
e2e/                black-box conformance suite (Python/pytest)
Dockerfile          one image build for all four services
```

The stack itself is defined by `docker-compose.yml` at the repo root, which
runs these four alongside MongoDB, RabbitMQ and the web client. Tasks are in the
root `justfile`.

The backend was originally written in Go and was rewritten in Rust; see
`rust/README.md` for what that involved and `e2e/README.md` for how it was kept
honest.

## Configuration

Every service reads its configuration from environment variables. Three are
security-critical:

- `INTERNAL_SECRET` — shared secret proving a request came through the gateway.
  **The gateway and all three backends must be given the same value, and every
  service refuses to start without it.** The backends trust the `X-Userid` /
  `X-Sessionid` headers the gateway injects without verifying them, so this
  header is what stops an accidentally exposed backend port from allowing
  anyone to impersonate any account. Generate one with `openssl rand -hex 32`.
- `JWT_SECRET` — signs session access tokens. Auth only.
- `ENCRYPTION_KEY` — exactly 32 bytes; encrypts provider OAuth tokens and
  fetched payloads at rest. Integration only. Changing it makes existing
  credentials undecryptable.

Only the gateway's port should ever be published. `INTERNAL_SECRET` is defence
in depth, not a substitute for keeping the backends on a private network.

Mongo must be a replica set: applying a sync update uses a transaction, which a
standalone mongod cannot serve. The compose stack runs a single-node set that
initiates itself from its healthcheck, so there is no manual `rs.initiate()`.

## Building images

From the repo root:

```bash
just up                    # build and run locally
just publish               # build and push to a registry
```

`publish` honours `REGISTRY` and `TAG`.

One `Dockerfile` serves all four services; `--build-arg SERVICE=<name>` selects
which binary lands in the runtime image. The builder stage takes no build args
and compiles the whole workspace, which is what lets BuildKit build the
dependency tree once and reuse it for the other three images. Building
per-service instead makes each image a separate cache entry, repeats the work
four times, and concurrently is enough memory to get cargo OOM-killed.

## Testing

`e2e/` holds a black-box conformance suite that runs all four services against
real Mongo and RabbitMQ and exercises them over HTTP -- 247 tests. It boots its
own throwaway infrastructure, so it never touches a running stack. See
`e2e/README.md`.

```bash
just test-e2e-setup    # first time only
just test-e2e
```

`just smoke` is the complementary check: it exercises the *deployed* compose
stack rather than the code, which is what catches a mismatched secret or a
broker nothing is bound to.
