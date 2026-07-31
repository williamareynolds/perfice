# Builds the web client and serves it with nginx.
#
# Build context is the repo root, not client/, because the client depends on
# the Capacitor plugin in ../android as a `file:` dependency:
#
#   docker build -f client/Dockerfile --build-arg VITE_BACKEND_URL=... .

FROM node:24-alpine AS build
WORKDIR /app

COPY android android
COPY client client

# The plugin has to be compiled before the client can resolve it: its
# package.json points `module` at dist/esm/index.js, and dist/ is generated
# rather than committed. Without this the client build fails with "Failed to
# resolve entry for package perfice-android".
#
# --ignore-scripts skips the swiftlint postinstall, which downloads a macOS-only
# binary and is only used for linting the iOS sources.
WORKDIR /app/android
RUN npm ci --ignore-scripts \
    && npx tsc \
    && npx rollup -c rollup.config.mjs

WORKDIR /app/client
RUN npm install

# Vite inlines this at build time, so the image is tied to one backend URL.
# It is only the *default*: the app can be pointed elsewhere at runtime from
# the globe icon in settings.
ARG VITE_BACKEND_URL="http://localhost:3000"
RUN npm run build

FROM nginx:alpine

RUN rm -rf /usr/share/nginx/html/*
COPY client/nginx.conf /etc/nginx/conf.d/default.conf
# Served under /new; see nginx.conf and the client's `base` in vite.config.ts.
COPY --from=build /app/client/dist /usr/share/nginx/html/new

EXPOSE 80
CMD ["nginx", "-g", "daemon off;"]
