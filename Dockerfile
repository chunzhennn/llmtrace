FROM node:22-bookworm AS ui
WORKDIR /ui
ENV CI=true
RUN corepack enable
COPY crates/llmtrace/ui/package.json crates/llmtrace/ui/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY crates/llmtrace/ui ./
RUN pnpm run build

FROM rust:1.96-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY --from=ui /ui/build ./crates/llmtrace/ui/build
RUN cargo build --locked --release -p llmtrace

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 llmtrace \
    && useradd --uid 10001 --gid llmtrace --home-dir /var/lib/llmtrace --create-home --shell /usr/sbin/nologin llmtrace
COPY --from=build /app/target/release/llmtrace /usr/local/bin/llmtrace
WORKDIR /var/lib/llmtrace
RUN install -d -o llmtrace -g llmtrace -m 0700 /var/lib/llmtrace/spool
EXPOSE 3000 3001
HEALTHCHECK --interval=30s --timeout=3s --start-period=30s --retries=3 \
    CMD curl -fsS "${LLMTRACE_HEALTHCHECK_URL:-http://127.0.0.1:3000/readyz}" || exit 1
USER llmtrace:llmtrace
ENTRYPOINT ["llmtrace"]
