FROM rust:1.69 AS builder
WORKDIR /usr/src/add-bot
COPY . .
RUN cargo install --path .

FROM debian:bullseye-slim
RUN apt-get update && apt-get install -y ca-certificates openssl && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/cargo/bin/lifx-mqtt /usr/local/bin/lifx-mqtt
CMD ["lifx-mqtt"]
