# bdc-search for UNC Cloud Apps (OpenShift): runs as an arbitrary non-root UID, reads the
# index from INDEX_DIR (mounted, or baked into a release image).

# bullseye's glibc (2.31) is older than UBI 9's (2.34), so the binary runs on either
FROM docker.io/library/rust:1.88-bullseye AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo build --release --locked

# pinned so a rebuild of the same commit gets the same base; bump deliberately
FROM registry.access.redhat.com/ubi9/ubi-minimal:9.9-1794467051@sha256:baafce2d34783a28b4a472779f304e97e00f9d35c99553bc53b068b14344d9a9
COPY --from=build /src/target/release/bdc-search /usr/local/bin/bdc-search
ENV INDEX_DIR=/index BIND=0.0.0.0:8080 RUST_LOG=info
USER 1001
EXPOSE 8080
ENTRYPOINT ["bdc-search", "serve"]
