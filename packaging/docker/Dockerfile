# ratatoskr as a container, built from source: the MCP server on stdio by
# default (`docker run -i --rm ghcr.io/haszerobyte/ratatoskr`), or any other
# command (`docker run --rm ghcr.io/haszerobyte/ratatoskr search "tokio"`).
#
# The release workflow publishes the same image from its prebuilt binaries
# (Dockerfile.release); this one is for building it yourself, from the
# repository root: docker build -f packaging/docker/Dockerfile .
# Dockerfile.dockerignore keeps build output and local secrets out of it.

FROM rust:1-bookworm AS build
# BoringSSL (through wreq) needs cmake and libclang to build.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake clang libclang-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
# rust-toolchain.toml pins the compiler; rustup installs it here.
RUN cargo build --release --locked --bin ratatoskr

# glibc and libgcc, nothing else: no shell, no package manager.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/ratatoskr /usr/local/bin/ratatoskr
LABEL org.opencontainers.image.source="https://github.com/haszerobyte/Ratatoskr" \
      org.opencontainers.image.description="Web search and page fetching for LLM agents: keyless meta-search, SSRF-safe fetch, MCP server" \
      org.opencontainers.image.licenses="Apache-2.0" \
      io.modelcontextprotocol.server.name="io.github.haszerobyte/ratatoskr"
ENTRYPOINT ["/usr/local/bin/ratatoskr"]
CMD ["mcp"]
