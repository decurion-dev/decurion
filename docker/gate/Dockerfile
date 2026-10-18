# The gate: the one way out of an agent's box, letting through only the
# websites on the app's list. Build from the repository root:
#   docker build -t decurion/gate:dev -f docker/gate/Dockerfile .
# Installed copies build from a folder holding only Cargo.toml, Cargo.lock
# and the proto and gate crates, so the workspace is cut down to those two.
# Cargo keeps the locked versions; it only drops the other crates' entries,
# which is why this build cannot use --locked.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN sed -i 's#^members = .*#members = ["crates/decurion-proto", "crates/decurion-gate"]#' Cargo.toml
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p decurion-gate \
 && cp target/release/decurion-gate /decurion-gate

# Distroless: no shell or package manager for anything to use, and a
# non-root user. The "cc" flavour has the C library the binary links to.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /decurion-gate /usr/local/bin/decurion-gate
USER nonroot
EXPOSE 3128
ENTRYPOINT ["/usr/local/bin/decurion-gate"]
