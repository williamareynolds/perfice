# server
Perfice can be run completely locally without a server, but using a server allows some extra features.  
The server component is responsible for synchronizing data between devices and pulling data automatically from integrations like Fitbit, Todoist into the platform.

More information can be read in the [docs](https://perfice.adoe.dev/docs/).

## Configuration

Every service reads its configuration from environment variables (a `.env` in
the working directory is loaded automatically). Two are security-critical:

- `INTERNAL_SECRET` — shared secret proving a request came through the gateway.
  **The gateway and all three backends must be given the same value, and every
  service refuses to start without it.** The backends trust the `X-Userid` /
  `X-Sessionid` headers the gateway injects without verifying them, so this
  header is what stops an accidentally exposed backend port from allowing
  anyone to impersonate any account. Generate one with `openssl rand -hex 32`.
- `JWT_SECRET` — signs session access tokens. Auth only.

Only the gateway's port should ever be published. `INTERNAL_SECRET` is defence
in depth, not a substitute for keeping the backends on a private network.

## Testing

`e2e/` holds a black-box conformance suite that runs all four services against
real Mongo and Kafka and exercises them over HTTP. See `e2e/README.md`.
