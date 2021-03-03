FROM rust:1.50-alpine AS builder

WORKDIR /ws

RUN apk add --no-cache \
    musl-dev \
    ca-certificates

ADD Cargo.toml ./
ADD src/ ./src

ENV PKG_CONFIG_ALL_STATIC=1
RUN cargo build --release

FROM scratch

COPY --from=builder /ws/target/release/eueoeo /
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/

ENTRYPOINT ["/eueoeo"]