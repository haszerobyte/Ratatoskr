# For MCP directories (Glama, Smithery, …) that look for a Dockerfile at the
# repository root to build and inspect a server: the published release image,
# so their build takes seconds instead of compiling BoringSSL. It runs the MCP
# server on stdio.
#
# To build ratatoskr from source, or the release image itself, see
# packaging/docker/.
FROM ghcr.io/haszerobyte/ratatoskr:latest
