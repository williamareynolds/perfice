![Perfice - Screenshot](https://raw.githubusercontent.com/p0lloc/perfice/main/client/assets/screenshot.png)

<div align="center">
  <h1>Perfice - Open self-tracking platform</h1>
  <a target="_blank" href="https://perfice.adoe.dev">
    Website
  </a> | 
  <a href="https://perfice.adoe.dev/docs">
    Documentation
  </a>
</div>  
<p></p>
Perfice is an open-source self-tracking platform that helps you track anything you like, and see how different metrics impact other areas of your life. It is built to be heavily customizable and local-first, leaving you in control of your tracking journey.


## Key Features
- **Trackables**: Easily track anything you like - sleep, mood, or food
- **Correlations**: Automatic insights like "Mood is better when you sleep longer"
- **Goals**: Set goals to stay motivated, supports multiple trackables
- **Privacy first**: Data is stored and calculated locally on your device
- **Exportability**: All data can be exported to/from CSV and JSON
## Quick Start

The whole thing -- client, backend, database, message broker -- runs from one
compose file. You need Docker and [just](https://github.com/casey/just).

```bash
just setup    # writes .env with freshly generated secrets
just up       # builds the images, starts everything, waits until it answers
just smoke    # optional: proves sync and account deletion actually work
```

Then open <http://localhost/new>.

The first `just up` compiles the Rust backend and takes several minutes;
afterwards it is seconds. `just` on its own lists every task.

Port 80 or 3000 already taken? Set `CLIENT_PORT` / `GATEWAY_PORT` in `.env` --
and if you move the client, add its new origin to `CORS_EXTRA_ORIGINS`, or the
browser will block every backend call.

### Client only

Perfice is local-first and fully usable with no backend at all; you only need
one for accounts, cross-device sync and integrations.

```bash
cd client
npm install
npm run dev
```

### Building the client for production

```bash
cd client && npm run build
```

The output in `client/dist` is static files, served under the `/new` subpath.

## Stack
Perfice is built with Svelte 5, TypeScript, TailwindCSS and leverages IndexedDB for most data storage.    
It uses [Capacitor](https://github.com/ionic-team/capacitor) to wrap the webapp in a native WebView application for Android.

### Running the Android app
```bash
CAPACITOR=true npm run build && npx cap run android
```

## Backend

Four Rust services behind a gateway -- `auth`, `sync`, `integration`, `gateway`
-- plus MongoDB and RabbitMQ. See [`server/README.md`](server/README.md) for the
architecture and [`server/rust/README.md`](server/rust/README.md) for the
internals.

The client picks its backend at runtime from the globe (🌐) icon in settings, so
one build can point anywhere. `PUBLIC_BACKEND_URL` in `.env` sets the default
that gets compiled in.
## License
Perfice is licensed under the [MIT license](https://github.com/p0lloc/perfice/blob/main/LICENSE).

## Contributing
Contributions are always appreciated and welcome! Please open an issue or pull request if you have any suggestions or find a bug.
