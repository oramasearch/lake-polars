# SPDX-License-Identifier: MIT

# lake-polars, powered by Polars: one statically linked musl binary on an empty (scratch) image,
# run as the numeric nonroot user 65532. The image holds the binary and nothing else: no libc, no
# shell, no package manager, so no GPL or LGPL code ships in it. The builder image is
# digest-pinned and never shipped.
#
#   docker buildx build --platform linux/amd64 -t lake-polars:<tag> .
#
# Build on a native host of the target architecture, never under emulation: Polars is a large
# compile. The builder's own architecture is the target (Alpine's Rust targets musl natively).
FROM rust:1.95-alpine@sha256:606fd313a0f49743ee2a7bd49a0914bab7deedb12791f3a846a34a4711db7ed2 AS builder
# musl-dev for the C parts (zstd, lz4, mimalloc), binutils for the static-link check; the Rust
# musl target links statically by default.
RUN apk add --no-cache musl-dev binutils
WORKDIR /workspace
# Dependencies first, so a source-only change reuses the (long) dependency layer.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && touch src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src
COPY src/ src/
RUN touch src/main.rs src/lib.rs && cargo build --release --locked \
    && cp target/release/lake-polars /lake-polars \
    && if readelf -d /lake-polars | grep -q NEEDED; then echo "not static" >&2; exit 1; fi \
    && mkdir -p /rootfs/data /rootfs/tmp && chown 65532:65532 /rootfs/data && chmod 1777 /rootfs/tmp

FROM scratch
COPY --from=builder /rootfs/ /
COPY --from=builder /lake-polars /lake-polars
USER 65532:65532
EXPOSE 8080
# Datasets live on the mounted volume; the operator's StatefulSet mounts it at /data.
ENV LAKE_POLARS_DIR=/data
ENTRYPOINT ["/lake-polars"]
