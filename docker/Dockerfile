FROM rust:1.97-alpine3.24@sha256:3c38f3f82c2f3d73da3b38e18d279393a04cb43ddded0e35088a8c3324d40900 AS builder
ARG TARGETARCH
WORKDIR /build
RUN apk add --no-cache build-base
ENV CARGO_PROFILE_RELEASE_OPT_LEVEL=z
COPY Cargo.toml Cargo.lock README.md README.en.md LICENSE-MIT LICENSE-APACHE ./
COPY crates ./crates
COPY src ./src
COPY zcf ./zcf
# 运行层不提供 GNU 动态加载器，安装到最终镜像前必须保持程序自包含。
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=naxus-registry-${TARGETARCH},sharing=locked \
    --mount=type=cache,target=/build/target,id=naxus-musl-size-${TARGETARCH},sharing=locked \
    cargo build --release --workspace --locked \
    && mkdir -p /build/out \
    && cp target/release/naxus target/release/cargo-naxus target/release/cargo-credential-naxus /build/out/ \
    && for binary in /build/out/*; do \
         readelf -h "$binary" >/dev/null; \
         if readelf -l "$binary" | grep -q INTERP || readelf -d "$binary" | grep -q NEEDED; then \
           echo 'Runtime binaries must be statically linked' >&2; exit 1; \
         fi; \
       done

FROM alpine:3.24.3@sha256:be2656061b536c1883c4034790e21d0a09c5aaaf51968b89368a3c0371ea2c9a AS runtime-files
RUN apk add --no-cache busybox-static ca-certificates-bundle tzdata
COPY docker/runtime-rootfs.sh /build-runtime-rootfs.sh
RUN sh /build-runtime-rootfs.sh

FROM scratch
ARG NAXUS_VERSION=1.0.0
ARG NAXUS_REVISION=local
LABEL org.opencontainers.image.title="Naxus" \
      org.opencontainers.image.description="Rust artifact repository with same-version publishing, immutable snapshots and public registry caching" \
      org.opencontainers.image.source="https://github.com/nasa-runtime/naxus" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0" \
      org.opencontainers.image.version="${NAXUS_VERSION}" \
      org.opencontainers.image.revision="${NAXUS_REVISION}"
COPY --from=runtime-files /rootfs/ /
COPY --from=builder /build/out/naxus /usr/local/bin/naxus
COPY zcf/application.yml /opt/naxus/zcf/application.yml
COPY LICENSE-MIT LICENSE-APACHE /usr/share/doc/naxus/
ENV PATH=/usr/local/bin:/bin \
    NAXUS_HOST=0.0.0.0 \
    NAXUS_DATA_PATH=/var/lib/naxus \
    NAXUS_LOG_PATH=/usr/local/logs/naxus \
    NAXUS_ACCESS_MODE=managed \
    NAXUS_PORT=7443 \
    TZ=Etc/UTC
WORKDIR /opt/naxus
USER 10001:10001
EXPOSE 7443
VOLUME ["/var/lib/naxus", "/usr/local/logs/naxus"]
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=30s --timeout=7s --start-period=30s --retries=3 CMD ["naxus", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/naxus"]
CMD ["serve"]
