# SPDX-License-Identifier: MIT

# lake-polars, powered by Polars: one Rust binary built against glibc on Debian 12 (bookworm), run
# on distroless/cc-debian12 (glibc and libgcc, no shell, no package manager) as the numeric
# nonroot user 65532. Both base images are digest-pinned; builder and runtime share Debian 12's
# glibc.
#
#   docker buildx build --platform linux/amd64 -t oramasearch/lake-polars:<tag> .
#
# Build on native amd64 (the cluster BuildKit), never under emulation: Polars is a large compile.
FROM rust:1.95-bookworm@sha256:6258907abe69656e41cd992e0b705cdcfabcbbe3db374f92ed2d47121282d4a1 AS builder
WORKDIR /workspace
# Dependencies first, so a source-only change reuses the (long) dependency layer.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && touch src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src
COPY src/ src/
RUN touch src/main.rs src/lib.rs && cargo build --release --locked \
    && cp target/release/lake-polars /lake-polars

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=builder /lake-polars /lake-polars
USER 65532:65532
EXPOSE 8080
# Datasets live on the mounted volume; the Orama operator's StatefulSet mounts it at /data.
ENV LAKE_POLARS_DIR=/data
ENTRYPOINT ["/lake-polars"]
