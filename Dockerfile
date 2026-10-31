# syntax=docker/dockerfile:1
# Static musl build on scratch, running as a non-root user (ADR-008).
FROM rust:1.99-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p spoolbox \
 && mkdir -p /out/tmp /out/data \
 && cp target/release/spoolbox /out/spoolbox

FROM scratch
COPY --from=build /out/spoolbox /spoolbox
COPY --from=build --chown=65532:65532 --chmod=1777 /out/tmp /tmp
COPY --from=build --chown=65532:65532 /out/data /data
USER 65532:65532
# Inside a container the network namespace is the trust boundary, so listen on all
# interfaces explicitly; the binary itself defaults to loopback only.
ENV SPOOLBOX_LISTEN=0.0.0.0:8025 \
    SPOOLBOX_SMTP=0.0.0.0:1025 \
    SPOOLBOX_ALLOW_UNAUTHENTICATED_NETWORK=true
EXPOSE 1025/tcp 8025/tcp
VOLUME ["/data"]
HEALTHCHECK --interval=15s --start-period=5s --timeout=3s CMD ["/spoolbox", "readyz"]
ENTRYPOINT ["/spoolbox"]
