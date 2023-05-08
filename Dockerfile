FROM gcr.io/distroless/cc
COPY target/x86_64-unknown-linux-musl/release/lifx-mqtt /usr/local/bin/lifx-mqtt
CMD ["lifx-mqtt"]
