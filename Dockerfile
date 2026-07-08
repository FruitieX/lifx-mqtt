FROM gcr.io/distroless/static@sha256:d5f030ca7c5793784e9ea4178a116da360250411d13921a5af27c6cb5a5949bf
COPY target/x86_64-unknown-linux-musl/release/lifx-mqtt /usr/local/bin/lifx-mqtt
CMD ["lifx-mqtt"]
