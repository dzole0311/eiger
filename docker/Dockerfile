FROM rust:1-bookworm AS chef
WORKDIR /app
RUN cargo install cargo-chef --locked

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --bin eiger-server --recipe-path recipe.json
COPY . .
RUN cargo build --release --bin eiger-server

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
  && apt-get install -y --no-install-recommends chromium ca-certificates dumb-init procps \
  && rm -rf /var/lib/apt/lists/*

ENV EIGER_BIND_ADDR=0.0.0.0:3000 \
    EIGER_CHROME_EXECUTABLE=/usr/bin/chromium \
    EIGER_CHROME_NO_SANDBOX=true \
    EIGER_STEALTH_ENABLED=true

COPY --from=builder /app/target/release/eiger-server /usr/local/bin/eiger

EXPOSE 3000
ENTRYPOINT ["dumb-init", "--"]
CMD ["eiger"]
