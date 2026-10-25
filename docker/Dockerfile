FROM rust:1.97-bookworm@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97 AS builder
ARG TARGETARCH
WORKDIR /build
COPY Cargo.toml Cargo.lock README.md README.en.md LICENSE-MIT LICENSE-APACHE ./
COPY crates ./crates
COPY src ./src
COPY zcf ./zcf
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=naxus-registry-${TARGETARCH},sharing=locked \
    --mount=type=cache,target=/build/target,id=naxus-target-${TARGETARCH},sharing=locked \
    cargo build --release --workspace --locked \
    && mkdir -p /build/out \
    && cp target/release/naxus target/release/cargo-naxus target/release/cargo-credential-naxus /build/out/

FROM debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8
ARG NAXUS_VERSION=1.0.0
ARG NAXUS_REVISION=local
LABEL org.opencontainers.image.title="Naxus" \
      org.opencontainers.image.description="Rust artifact repository with same-version publishing, immutable snapshots and public registry caching" \
      org.opencontainers.image.source="https://github.com/nasa-runtime/naxus" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0" \
      org.opencontainers.image.version="${NAXUS_VERSION}" \
      org.opencontainers.image.revision="${NAXUS_REVISION}"
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 naxus \
    && useradd --uid 10001 --gid 10001 --home-dir /var/lib/naxus --no-create-home --shell /usr/sbin/nologin naxus \
    && mkdir -p /opt/naxus/zcf /var/lib/naxus /usr/local/logs/naxus /var/backups/naxus \
    && chown -R 10001:10001 /var/lib/naxus /usr/local/logs/naxus /var/backups/naxus
COPY --from=builder /build/out/naxus /usr/local/bin/naxus
COPY zcf/application.yml /opt/naxus/zcf/application.yml
COPY LICENSE-MIT LICENSE-APACHE /usr/share/doc/naxus/
ENV NAXUS_HOST=0.0.0.0 \
    NAXUS_DATA_PATH=/var/lib/naxus \
    NAXUS_LOG_PATH=/usr/local/logs/naxus \
    NAXUS_ACCESS_MODE=managed \
    NAXUS_PORT=7443
WORKDIR /opt/naxus
USER 10001:10001
EXPOSE 7443
VOLUME ["/var/lib/naxus", "/usr/local/logs/naxus"]
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=30s --timeout=7s --start-period=30s --retries=3 CMD ["naxus", "healthcheck"]
ENTRYPOINT ["naxus"]
CMD ["serve"]
