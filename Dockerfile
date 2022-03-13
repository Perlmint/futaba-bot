FROM rust:1.59-alpine AS builder

WORKDIR /ws

RUN apk add --no-cache \
    musl-dev \
    ca-certificates \
    openssl-dev

RUN cargo install sqlx-cli

ADD Cargo.toml ./
ADD src/ ./src

ADD Cargo.toml ./
ADD src/ ./src
ADD migrations/ ./migrations
ADD .env ./

ENV PKG_CONFIG_ALL_STATIC=1
RUN sqlx database create && sqlx migrate run
RUN cargo build --release

FROM scratch

COPY --from=builder /ws/target/release/futaba /
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/

ENTRYPOINT ["/futaba"]