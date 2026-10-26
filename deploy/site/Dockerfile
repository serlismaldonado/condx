# syntax=docker/dockerfile:1.7
# El sitio en la nube de Condx (docs/design/cloud-site.md, fase S1): condxd en Linux con el host de
# agentes, en modo sitio (sin producción ni Llavero; secretos en /data/condx/secrets.json).
#
#   docker build -f deploy/site/Dockerfile -t condx-site .
#   docker run -d --name condx-site --network host -v condx-site:/data condx-site   # host: boat publica sus puertos
#   docker exec condx-site condxd site pair "<the QR's text or link>"
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release -p devtool-daemon && cp target/release/condxd /condxd

FROM node:22-bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends git ca-certificates curl unzip \
    && rm -rf /var/lib/apt/lists/*
# Los servicios locales en la nube (el «local» con la Mac apagada): el backend de Convex para Linux
# (versión fija) y Bun para instalar las dependencias de los proyectos que lo usan.
ARG CONVEX_BACKEND=precompiled-2026-10-10-72ce13c
ARG BUN_VERSION=bun-v1.2.23
RUN arch=$(uname -m | sed 's/arm64/aarch64/') \
    && curl -fsSL -o /tmp/backend.zip "https://github.com/get-convex/convex-backend/releases/download/${CONVEX_BACKEND}/convex-local-backend-${arch}-unknown-linux-gnu.zip" \
    && unzip -q /tmp/backend.zip -d /usr/local/bin && chmod +x /usr/local/bin/convex-local-backend && rm /tmp/backend.zip \
    && curl -fsSL https://bun.sh/install | BUN_INSTALL=/usr/local bash -s "${BUN_VERSION}"
# Claude Code para los agentes con el proveedor «Claude Code» (versión fija). Se autentica con el token
# de `claude setup-token` que manda la Mac (no hay sesión interactiva en el sitio).
ARG CLAUDE_CODE_VERSION=2.1.296
RUN npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" && npm cache clean --force
WORKDIR /opt/condx/agent-host
COPY agent-host/package.json agent-host/package-lock.json ./
RUN npm ci --omit=dev && npm cache clean --force
COPY agent-host/src ./src
COPY --from=build /condxd /usr/local/bin/condxd
ENV HOME=/data CONDX_HOME=/data/condx CONDX_SITE=cloud CONDX_AGENT_HOST_DIR=/opt/condx/agent-host CONDX_CONVEX_BACKEND=/usr/local/bin/convex-local-backend
VOLUME /data
WORKDIR /data
CMD ["condxd", "serve", "--site", "cloud"]
