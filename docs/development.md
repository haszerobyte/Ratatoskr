# Development

## Building from source

The Rust toolchain version is pinned in `rust-toolchain.toml`; rustup picks
it up automatically. wreq links BoringSSL, which needs:

| OS | Install |
|---|---|
| Debian / Ubuntu | `sudo apt install cmake clang libclang-dev` |
| Fedora | `sudo dnf install cmake clang-devel` |
| macOS | `brew install cmake`, plus Xcode's command-line tools |
| Windows | Visual Studio Build Tools, CMake, [NASM](https://nasm.us) and LLVM (`LIBCLANG_PATH`) |

```sh
git clone https://github.com/haszerobyte/Ratatoskr
cd Ratatoskr
cargo build --release          # target/release/ratatoskr
cargo test                     # offline: tests use recorded pages and local fake servers
```

CI builds and tests on Linux (x86_64, aarch64), macOS (Intel, Apple Silicon)
and Windows.

## Contributing

Issues and pull requests are welcome. Before opening one, run:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

### Repository layout

| Path | What's there |
|---|---|
| [`src/`](../src) | the library; [`src/bin/ratatoskr.rs`](../src/bin/ratatoskr.rs) is the command, [`src/mcp.rs`](../src/mcp.rs) the MCP server |
| [`src/engine/`](../src/engine) | the web search engines and the meta-search that combines them |
| [`src/vertical/`](../src/vertical) | the news, images, videos and books searches |
| [`tests/`](../tests) | integration tests; [`tests/fixtures/`](../tests/fixtures) holds recorded engine pages and trimmed real pages |
| [`packaging/docker/`](../packaging/docker) | the container image: built from source, and from the release binaries |
| [`Dockerfile`](../Dockerfile) | for MCP directories that build a server from the repository root: the published image |
| [`packaging/mcp/`](../packaging/mcp) | [`server.json`](../packaging/mcp/server.json), the MCP Registry entry |
| [`bench/`](../bench) | the benchmark against other tools |
| [`.github/workflows/`](../.github/workflows) | CI and the release |

### When a search engine changes its page

Search engines change their pages. When an engine stops parsing:
1. capture a fresh results page into [`tests/fixtures/`](../tests/fixtures);
2. replace its per-request tracking tokens with `TOKEN`;
3. adjust the engine's parser.

## Releasing

Releases are published by the **Release** workflow, and only after CI
passes. Run **Actions → Release → Run workflow** on `main` with the new
version, e.g. `0.1.1`. There's no need to edit `Cargo.toml` first.

The workflow runs these steps in order, stopping at the first failure:
1. checks that the version is a semantic version, isn't below `Cargo.toml`'s,
   and isn't released on GitHub or crates.io yet;
2. writes it into `Cargo.toml` and `Cargo.lock`, and commits that to `main`
   as "Release <version>";
3. checks that the crate packages (`cargo publish --dry-run`);
4. runs the full CI on that commit, on every platform;
5. builds the binaries;
6. pushes the container image of the Linux binaries to
   `ghcr.io/haszerobyte/ratatoskr` (`<version>`, and `latest` unless it's a
   pre-release);
7. drafts the GitHub release with the binaries and their SHA-256 checksums,
   so a GitHub-side failure stops it before anything is published;
8. publishes `ratatoskr-web` to crates.io;
9. tags the commit `v<version>` and publishes the drafted release;
10. updates the [MCP Registry](https://registry.modelcontextprotocol.io)
    entry from [`packaging/mcp/server.json`](../packaging/mcp/server.json), pointing at that version's image. This runs
    last, on its own: if it fails, the release is still complete.

The first image push creates the `ratatoskr` package on GitHub as private:
make it public once in its package settings, or `docker pull` and the
registry entry won't work for anyone else.

It needs these repository secrets:
- **`CARGO_REGISTRY_TOKEN`** (required): a crates.io API token with the
  publish-new and publish-update scopes.
- **`RELEASE_TOKEN`** (optional): a fine-grained token with Contents read and
  write on this repository only, used for the GitHub release. Without it, the
  workflow's own token is used.
