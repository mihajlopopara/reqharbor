# syntax=docker/dockerfile:1
# ReqHarbor server image (docs/plans/accounts/06-go-server.md §7.1).
# Build context is server/:  docker build -f server/Dockerfile -t reqharbor-server:dev server
# No build tags are passed, so the dev sign-in provider (//go:build devauth) is never in the image.

# Go release: keep the tag and digest in step with the `go` service in compose.yaml.
FROM --platform=$BUILDPLATFORM golang:1.27.4-bookworm@sha256:ce59b41415e2524ae99926fe1f37ce48ba8c194dcbd2a56a65436cdcefa2bd34 AS build
ARG TARGETOS TARGETARCH VERSION=dev COMMIT=unknown
WORKDIR /src
COPY go.mod go.sum ./
RUN --mount=type=cache,target=/go/pkg/mod go mod download
COPY . .
RUN --mount=type=cache,target=/go/pkg/mod --mount=type=cache,target=/root/.cache/go-build \
    CGO_ENABLED=0 GOOS=$TARGETOS GOARCH=$TARGETARCH \
    go build -trimpath -ldflags="-s -w -X main.version=${VERSION} -X main.commit=${COMMIT}" \
      -o /out/reqharbor-server ./cmd/reqharbor-server \
 && mkdir -p -m 0700 /out/root/data

# Distroless static, non-root; CA certificates and tzdata included.
FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
COPY --from=build /out/reqharbor-server /reqharbor-server
# Copy the parent, so /data keeps its 0700 mode (copying the directory itself copies only its
# contents into a 0755 /data). The database holds password and token hashes.
COPY --from=build --chown=65532:65532 /out/root/ /
ENV REQHARBOR_DATA_DIR=/data REQHARBOR_LISTEN=:8080
VOLUME ["/data"]
EXPOSE 8080
USER 65532:65532
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 CMD ["/reqharbor-server","healthcheck"]
ENTRYPOINT ["/reqharbor-server"]
CMD ["serve"]
LABEL org.opencontainers.image.source="https://github.com/mihajlopopara/reqharbor" \
      org.opencontainers.image.title="reqharbor-server" \
      org.opencontainers.image.description="Self-hostable ReqHarbor sync and share server" \
      org.opencontainers.image.licenses="MIT"
