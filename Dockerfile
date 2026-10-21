# syntax=docker/dockerfile:1
# The server image: lager, the web interface, Git, and tini.
#
#   docker build -t lager:$(git rev-parse --short HEAD) .
#
# Cache mounts keep the Bun cache, the Cargo registry, and the Cargo target
# directory between builds on the same machine.

FROM oven/bun:1.4-slim AS web
WORKDIR /src/web
COPY web/package.json web/bun.lock ./
RUN --mount=type=cache,target=/root/.bun/install/cache bun install --frozen-lockfile
COPY web/ ./
RUN bun run build

# Keep in step with rust-toolchain.toml.
FROM rust:1.98.1-slim-trixie AS rust
# The toolchain in the image, without the components rust-toolchain.toml
# adds for development.
ENV RUSTUP_TOOLCHAIN=1.98.1
# webauthn-rs links OpenSSL.
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin lager --bin lager-server \
    && install -D -t /out target/release/lager target/release/lager-server

# Debian trixie has Git 2.47; the server stops at start with an older Git.
# libssl3t64 for webauthn-rs.
# tini is PID 1 because Git starts processes that lager does not reap
# when they are orphaned; it forwards SIGTERM for the clean stop.
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends git tini ca-certificates libssl3t64 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -g 10001 lager \
    && useradd -u 10001 -g 10001 -d /var/empty -s /usr/sbin/nologin lager \
    && install -d -o 10001 -g 10001 /var/cache/lager
# The server serves the web interface from share/lager/web beside the
# programs.
COPY --from=rust /out/ /usr/local/bin/
COPY --from=web /src/web/dist/client /usr/local/share/lager/web
USER 10001:10001
EXPOSE 8080 2222
VOLUME /var/cache/lager
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/lager-server"]
