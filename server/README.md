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
build.sh, push.sh   image build and publish
docker-compose.yml  deployment reference
```

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
standalone mongod cannot serve.

## Building images

```bash
./build.sh                 # all four
./build.sh auth sync       # just these
./push.sh                  # publish
```

Both honour `REGISTRY` and `TAG`.

## Testing

`e2e/` holds a black-box conformance suite that runs all four services against
real Mongo and Kafka and exercises them over HTTP. See `e2e/README.md`.
