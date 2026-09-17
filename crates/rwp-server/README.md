# rwp-server

Remote Workspace Protocol server for the harness. It exposes sessions, filesystem operations, edit primitives, bash execution, SQLite access, and named tunnels for eval/LSP/DAP/CDP over HTTP + WebSocket.

See [`docs/rwp-protocol.md`](../../docs/rwp-protocol.md) for the wire-level protocol notes and [`src/router.rs`](src/router.rs) for the endpoint source of truth.

## Build

```sh
cargo build -p rwp-server
```

## Run

```sh
cargo run -p rwp-server --bin rwp-server -- --bind 127.0.0.1:0
```

Example with an explicit port and informational workspace root:

```sh
cargo run -p rwp-server --bin rwp-server -- --bind 127.0.0.1:8765 --workspace-root /workspace
```

The server prints `rwp-server listening on http://...` on stdout after bind.

## CLI flags

- `--bind <ADDR>`: bind address, default `127.0.0.1:0`
- `--workspace-root <PATH>`: optional informational root; sessions still choose their own cwd

## Endpoint surface

Session-scoped endpoints:

- `POST /sessions`
- `DELETE /sessions/{id}`
- `PUT /sessions/{id}/cwd`
- `PATCH /sessions/{id}/env`
- `GET /sessions/{id}/events`
- `GET /sessions/{id}/read.lines`
- `GET /sessions/{id}/read.blob`
- `GET /sessions/{id}/read.ast`
- `PUT /sessions/{id}/write.lines`
- `PUT /sessions/{id}/write.blob`
- `GET /sessions/{id}/glob`
- `GET /sessions/{id}/grep`
- `GET /sessions/{id}/grep.ast`
- `POST /sessions/{id}/edit.replace`
- `POST /sessions/{id}/edit.patch`
- `POST /sessions/{id}/edit.ast`
- `POST /sessions/{id}/bash.exec`
- `GET /sessions/{id}/read.db`
- `POST /sessions/{id}/write.db`

Named handles:

- `GET|PUT|DELETE|POST /eval/{name}`
- `GET|PUT|DELETE /lsp/{name}` with `GET` also serving WS upgrades
- `GET|PUT|DELETE /dap/{name}` with `GET` also serving WS upgrades
- `GET|PUT|DELETE /cdp/{name}` with `GET` also serving WS upgrades

Schema:

- `GET /openapi.json`

## Architecture decisions

- Anchor semantics stay in the harness. The server stores bytes, emits `ETag`s, and applies edits against current file contents.
- `FileReadCache` is server-side because LSP document lifecycle needs a stable current snapshot and repeated reads should coalesce around the same cached bytes/etag.
- Bash runs through the in-process `pi-shell` / `brush-core` shell, so session shell state persists across calls.
- Named handles are server-owned and long-lived. They use registry entries with refcounts plus idle reapers; websocket attachment bumps the refcount and idle handles are reaped after inactivity.
- There is no `/jobs` registry. Long-running work is tied to the request or websocket lifetime; cancellation is implicit when the client disconnects. For bash, timeout-based cancellation is the supported path today, but background grandchildren are a documented limitation; see the protocol doc.
- LSP write-through lives in the shared write path, so `write.*`, `edit.*`, and AST edits all drive the same `didOpen`/`didChange` forwarding and `FileChanged` event emission.

## Verification

Local server checks:

```sh
bun run rwp
```

That runs:

```sh
cargo test -p rwp-server
cargo clippy -p rwp-server --all-targets -- -D warnings
```
