---
id: Self hosted
sidebar_position: 1
---

# Quick start
Perfice can run completely without a backend, since it's built as a local-first web application. However, you might want to run a backend if you're interested in the following:
- Sync: Synchronizing your data between devices (such as your phone)
- Integrations: Automatically fetching data from remote providers like Fitbit, Todoist, etc

### Running everything
The whole stack -- client, the four backend services, MongoDB and RabbitMQ --
comes from a single compose file at the repository root. You need Docker and
[just](https://github.com/casey/just).

```shell
just setup    # writes .env with freshly generated secrets
just up       # builds the images and starts everything
```

Then open <http://localhost/new>. `just` on its own lists every available task.

The first build compiles the Rust backend and takes several minutes. After that
it is seconds, since the dependency tree is cached.

##### Building from source
You can also build the client from source to produce static HTML/CSS/JS files.
```shell
cd client
npm install
npm run build
```
The built files will be placed in the `dist` folder, ready to be served by your favorite HTTP server. 

Keep in mind that the client is expected to be ran under the `/new` subpath. This might require you to tweak your configuration or place all files under a dummy `new` directory.

### Configuration
Everything is configured through `.env` at the repository root; `just setup`
generates one from `.env.example` with real secrets. The values you are most
likely to change are `PUBLIC_BACKEND_URL` and `PUBLIC_APP_URL` when moving off
localhost, and `CLIENT_PORT` / `GATEWAY_PORT` if those ports are taken.

Three secrets matter:

- `INTERNAL_SECRET` — proves a request came through the gateway. All four
  services must share the same value. The backends trust the identity headers
  the gateway injects, so this is what stops anyone who can reach a backend port
  from impersonating any account.
- `JWT_SECRET` — signs session tokens.
- `ENCRYPTION_KEY` — exactly 32 bytes; encrypts provider OAuth tokens and
  fetched data at rest. Changing it makes what is already stored unreadable.

Only the client and gateway publish ports. Keep it that way: the other services
are reachable on the internal network only, by design.

### Verifying it works
```shell
just smoke
```

This registers an account, syncs an update between two devices, deletes the
account and checks every trace of it was purged — which exercises the database,
the gRPC calls and the message broker in one pass.

## Architecture
The backend is four Rust services -- `gateway`, `auth`, `sync` and
`integration`. Only the gateway is publicly reachable; it authenticates requests
and forwards them inward. The services talk to each other over gRPC, and publish
events (account deletion, timezone changes) over RabbitMQ for the services that
need to react to them.