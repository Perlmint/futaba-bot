FROM rust:1.75-alpine AS builder

WORKDIR /ws

RUN apk add --no-cache \
    musl-dev \
    ca-certificates \
    openssl-dev \
    openssl-libs-static && \
    update-ca-certificates

RUN cargo install sqlx-cli --no-default-features --features sqlite,sqlx/runtime-tokio-rustls 

ADD Cargo.toml ./
ADD src/ ./src

ADD Cargo.toml ./
ADD src/ ./src
ADD migrations/ ./migrations
ADD .env ./

ENV PKG_CONFIG_ALL_STATIC=1
RUN sqlx database create && sqlx migrate run
RUN cargo build --release
RUN chmod 777 /ws/target/release/futaba

FROM scratch

COPY --from=builder /ws/target/release/futaba /app/
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/

EXPOSE 80
WORKDIR /app
ENTRYPOINT ["/app/futaba"]
