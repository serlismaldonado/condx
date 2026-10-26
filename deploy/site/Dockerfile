# syntax=docker/dockerfile:1.7
# El sitio en la nube de Condx (docs/design/cloud-site.md, fase S1): condxd en Linux con el host de
# agentes, en modo sitio (sin producción ni Llavero; secretos en /data/condx/secrets.json).
#
#   docker build -f deploy/site/Dockerfile -t condx-site .
#   docker run -d --name condx-site -v condx-site:/data condx-site
#   docker exec condx-site condxd site pair "<the QR's text or link>"
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release -p devtool-daemon && cp target/release/condxd /condxd

FROM node:22-bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends git ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /opt/condx/agent-host
COPY agent-host/package.json agent-host/package-lock.json ./
RUN npm ci --omit=dev && npm cache clean --force
COPY agent-host/src ./src
COPY --from=build /condxd /usr/local/bin/condxd
ENV HOME=/data CONDX_HOME=/data/condx CONDX_SITE=cloud CONDX_AGENT_HOST_DIR=/opt/condx/agent-host
VOLUME /data
WORKDIR /data
CMD ["condxd", "serve", "--site", "cloud"]
