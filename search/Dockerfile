# bdc-search for UNC Cloud Apps (OpenShift): runs as an arbitrary non-root UID, reads the
# index from INDEX_DIR (mounted, or baked into a release image).

# bullseye's glibc (2.31) is older than UBI 9's (2.34), so the binary runs on either
FROM docker.io/library/rust:1.88-bullseye AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo build --release --locked

FROM registry.access.redhat.com/ubi9/ubi-minimal:latest
COPY --from=build /src/target/release/bdc-search /usr/local/bin/bdc-search
ENV INDEX_DIR=/index BIND=0.0.0.0:8080 RUST_LOG=info
USER 1001
EXPOSE 8080
ENTRYPOINT ["bdc-search", "serve"]
