FROM gcr.io/distroless/static@sha256:9197324ba51d9cd071af8505989365c006adf9d6d2067eada25aef00abbb5278
COPY target/x86_64-unknown-linux-musl/release/lifx-mqtt /usr/local/bin/lifx-mqtt
CMD ["lifx-mqtt"]
