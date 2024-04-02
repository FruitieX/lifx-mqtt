FROM gcr.io/distroless/static@sha256:6d31326376a7834b106f281b04f67b5d015c31732f594930f2ea81365f99d60c
COPY target/x86_64-unknown-linux-musl/release/lifx-mqtt /usr/local/bin/lifx-mqtt
CMD ["lifx-mqtt"]
