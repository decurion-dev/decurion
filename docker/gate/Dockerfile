# The gate: the one way out of an agent's box, letting through only the
# websites on the app's list. Build from the repository root:
#   docker build -t decurion/gate:dev -f docker/gate/Dockerfile .
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p decurion-gate \
 && cp target/release/decurion-gate /decurion-gate

# Distroless: no shell or package manager for anything to use, and a
# non-root user. The "cc" flavour has the C library the binary links to.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /decurion-gate /usr/local/bin/decurion-gate
USER nonroot
EXPOSE 3128
ENTRYPOINT ["/usr/local/bin/decurion-gate"]
