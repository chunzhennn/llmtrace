FROM rust:1.96-bookworm AS build
WORKDIR /app
COPY Cargo.toml ./
COPY crates ./crates
RUN cargo build --release -p llmtrace

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/llmtrace /usr/local/bin/llmtrace
WORKDIR /var/lib/llmtrace
EXPOSE 3000
HEALTHCHECK --interval=30s --timeout=3s --start-period=30s --retries=3 \
    CMD curl -fsS http://127.0.0.1:3000/readyz || exit 1
ENTRYPOINT ["llmtrace"]
