# syntax=docker/dockerfile:1
# The image ghcr.io/teslacloud/fpkg: the fpkg binary, statically linked against
# musl, on Alpine. .github/workflows/image.yml builds and publishes it.
#
# The build needs the nl repository as the build context "nl" and the nl fork
# of LuaJIT prepared in vendor/luajit-src:
#
#   scripts/prepare-nl.sh
#   docker buildx build --build-context nl=../nl -t fpkg .
FROM rust:1-alpine AS build
RUN apk add --no-cache gcc make musl-dev

# The layout the manifests expect: fpkg and nl side by side. Of nl, the build
# reads the workspace manifest, the crates and the files they embed (the VM
# comes from vendor/).
COPY --from=nl Cargo.toml /src/nl/Cargo.toml
COPY --from=nl crates /src/nl/crates
COPY --from=nl decl /src/nl/decl
COPY --from=nl runtime /src/nl/runtime
WORKDIR /src/fpkg
COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/fpkg/target \
    CARGO_PROFILE_RELEASE_STRIP=symbols cargo build --release --locked -p fpkg && \
    cp target/release/fpkg /usr/local/bin/fpkg
# The binary evaluates specs on this architecture and libc.
RUN fpkg --version && fpkg --project tests/fixtures/flux-ce check --quiet

FROM alpine:3
LABEL org.opencontainers.image.title="fpkg" \
      org.opencontainers.image.description="fpkg, the package manager for Flux packages" \
      org.opencontainers.image.source="https://github.com/TeslaCloud/fpkg" \
      org.opencontainers.image.licenses="MIT"

COPY --from=build /usr/local/bin/fpkg /usr/local/bin/fpkg
# The package cache, writable by whichever user runs the image.
RUN mkdir -m 1777 /var/cache/fpkg
ENV FPKG_CACHE_DIR=/var/cache/fpkg

# Mount the project (or the server folder) here.
WORKDIR /work
ENTRYPOINT ["fpkg"]
CMD ["--help"]
