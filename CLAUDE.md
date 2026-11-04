# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A [Tabularis](https://github.com/TabularisDB/tabularis) driver plugin, written in Rust, that lets Tabularis connect to and query Elasticsearch clusters. Tabularis launches the compiled binary as a subprocess and talks to it over stdio using JSON-RPC (one JSON object per line in, one JSON object per line out). The plugin has no server of its own and no persistent state beyond an in-process connection-transport cache.

Full plugin contract (required RPC methods, slot names, manifest schema) lives in the upstream guide: `https://github.com/TabularisDB/tabularis/blob/main/plugins/PLUGIN_GUIDE.md`. Several doc comments in this repo link to specific sections of it.

## Commands

All common tasks are `just` recipes (see `justfile`); most just wrap `cargo`.

```bash
just build          # cargo build (debug) + builds ui/ if ui/package.json exists
just release         # cargo build --release + build-ui
just test            # cargo test
just lint            # cargo clippy --all-targets -- -D warnings
just fmt             # cargo fmt --all
just repl            # cargo run --bin test_plugin — local JSON-RPC sandbox (see caveat below)
just dev-install      # build + copy binary/manifest/ui bundle into the local Tabularis plugins dir
just uninstall        # remove the installed plugin
```

Run a single test: `cargo test <test_name>` (e.g. `cargo test execute_esql_success`). Doctests live inline in `utils/identifiers.rs` and `utils/pagination.rs`.

### Live-Elasticsearch tests

Most tests under `src/es/client.rs` and `src/handlers/query.rs` are **not mocked** — they open a real connection to `http://elastic:secret@123@localhost:9200` and exercise it. Before running `just test` / `cargo test`, start and seed a local cluster:

```bash
just run-es    # docker run elasticsearch:9.1.3, single-node, security enabled, password secret@123
just seed-es   # creates the "posts" index from testdata/ and bulk-loads it
```

Without a running cluster at that URL, these tests fail on connection, not on assertion.

Write a test that talks to Elasticsearch as a plain `#[test]` that runs its body through `test_support::block_on`, not as `#[tokio::test]`. `es::pool` caches connections for the whole process, and a connection belongs to the runtime that opened it; with a runtime per test, a test can pick up a connection whose runtime ended with an earlier test and fail with "runtime dropped the dispatch task".

### `just repl` caveat

`src/bin/test_plugin.rs` does **not** actually dispatch into `rpc::handle_line` — `main.rs`'s modules aren't shared with this bin target, so `simulate()` just echoes the request back as JSON. It's useful for checking request shapes, not for exercising handler logic. To test real dispatch end-to-end, pipe JSON-RPC lines into the main binary directly (`cargo run` then type a line like `{"jsonrpc":"2.0","method":"get_tables","params":{"params":{"database":"http://elastic:secret@123@localhost:9200"}},"id":1}`).

## Architecture

### Request flow

`main.rs` reads newline-delimited JSON from stdin, hands each line to `rpc::handle_line`, writes the serialized response (or a JSON-RPC error) back to stdout, one line per request. `rpc::handle_line` is a flat `match` on the `method` field that dispatches to a handler in `src/handlers/*`; unrecognized/unimplemented methods fall through to `not_implemented` (JSON-RPC `MethodNotFound`). Several methods in the match arm are commented out (CRUD, DDL, `explain_query`) — that's deliberate scaffolding for future work, not dead code to delete.

### Connection model — no `host`/`port`/`user`/`pass` fields

Unlike a typical SQL driver, this plugin does not build a connection string from discrete fields. Tabularis stores the **entire** Elasticsearch URL (e.g. `http://localhost:9200`) in the connection's `database` field, and `utils::extractor::extract_endpoint` pulls it out of `params.params.database` together with `username` and `password` (Tabularis fills `password` from the keychain when the connection has `save_in_keychain`). The credentials go out as HTTP Basic auth (`es::pool::Endpoint`), so they never appear in the URL; a URL that still holds `user:pass@` keeps working. `models::ConnectionParams` (host/port/database/username/password/ssl_mode) exists as a shared shape but is largely unused by the actual handlers — don't assume it's the source of truth for connecting.

`es::pool::get_transport` builds the `elasticsearch::http::transport::Transport` manually via `TransportBuilder` + `SingleNodeConnectionPool` instead of the `Transport::single_node(url)` convenience constructor, specifically so that URLs with literal `@` in the password (`http://user:pa@ss@host:9200`) parse correctly.

### Connection pooling

`es::pool` keeps a process-global `Mutex<HashMap<String, CachedTransport>>` keyed by the full connection URL string. On a miss it reads the server version (`GET /`) without holding the lock, and caches the transport together with that version; a version that cannot be read is not cached, so the next call probes again. Transports are reused across RPC calls for the same URL and evicted after a 30-minute idle TTL by a background tokio task spawned in `main.rs` (ticks every 10 minutes). `es::client::Client::from_url` always goes through this cache — never construct `Elasticsearch`/`Transport` directly in a handler.

### Query modes

Tabularis sends the query text as a single string; the plugin infers how to run it from an optional shebang-style prefix on the first line, parsed by `handlers::models::Query::from(String)`:

| Prefix | `QueryMode` | Executed via |
|---|---|---|
| _(none)_ or `#!sql` | `Sql` / `None` | By server version (`SqlFlavor::for_version`): ≥ 6.3 `Client::execute_sql` — X-Pack SQL (`_sql`); < 6.3 `Client::execute_nlpchina_sql` — the NLPchina `sql` plugin, which answers with a search response |
| `#!esql` | `Esql` | `Client::execute_esql` — ES\|QL (`_esql`) |
| `#!rest` | `Rest` | `Client::search` — raw REST, first line is `METHOD /path`, rest is the JSON body |

The driver is read-only on its own, independent of Tabularis's read-only mode: X-Pack SQL and ES|QL cannot write; `Client::search` only allows `GET`/`POST` to paths that are or end in `/_search` — it's not a general REST passthrough; and since the NLPchina plugin also runs `DELETE`, `execute_nlpchina_sql` only sends statements whose first word is `SELECT`/`SHOW`/`DESCRIBE`/`DESC`.

X-Pack SQL rejects a whole query when any selected field holds an array ("Arrays (returned by [x]) are not supported"). From 6.7 on, `execute_sql` sends `field_multi_value_leniency: true`, so such a field shows its first value; the full array is only visible through `#!rest`.

`execute_query` caps the result at the `limit` Tabularis sends (the MCP `run_query` limit, or the editor's page size) and sets `truncated` when rows were left out; `page` is ignored. Only X-Pack SQL can ask the server for fewer rows (`fetch_size`); when rows remain it gets a cursor back, which `execute_sql` closes at once since the plugin never fetches the next page. The other modes return the server's own default amount (ES|QL 1000 rows, NLPchina 200 hits, a search its `size`) and are cut afterwards.

A `SearchResponse` (NLPchina SQL and `#!rest`) becomes a table in `handlers::table`: with non-empty `aggregations`, one row per innermost bucket (bucket keys outer to inner, then the metrics; `doc_count` when a bucket has no metric); otherwise one row per hit, under the union of the fields of every hit. `handlers::query::QueryExecutor::execute` matches on `QueryMode` and converts each ES response type (`SqlResponse` / `EsqlResponse` / `SearchResponse`) into the driver-agnostic `ExecuteQueryResponse` (`columns`, `rows`, `affected_rows`, `execution_time_ms`) via `From` impls in `handlers::models`.

### Module layout

- `src/main.rs` — stdio read/dispatch/write loop + pool-cleanup background task
- `src/rpc.rs` — method-name → handler dispatch table, `ok_response`/`error_response` helpers
- `src/models.rs` — `ConnectionParams` (mostly unused, see above) and `inner_params` helper
- `src/error.rs` — `PluginError`/`ErrorCode` (JSON-RPC error codes); no `anyhow`/`thiserror` by design, keep deps light
- `src/es/client.rs` — `Client`, one method per ES operation (ping, cat indices, get mapping, SQL/ES\|QL/search, SQL translate)
- `src/es/pool.rs` — global transport cache (see above)
- `src/es/models.rs` — `serde` shapes for ES API responses (`Hit`, `SearchResponse`, `SqlResponse`, `IndexMapping`, ...)
- `src/handlers/query.rs` — `test_connection`/`ping` (currently just pings ES — no deeper validation) and `execute_query`
- `src/handlers/metadata.rs` — `get_tables` (cat indices), `get_columns` (index mapping → flattened column list); `get_routines`/`get_views`/`get_foreign_keys`/`get_indexes` are stubbed to `[]` directly in `rpc.rs` since ES has no equivalent concepts
- `src/handlers/crud.rs`, `src/handlers/ddl.rs` — stubs returning `not_implemented`; wired up but commented out in `rpc.rs`'s match. The manifest sets `capabilities.readonly: true`, so leaving these unimplemented is intentional, not a gap
- `src/test_support.rs` — `block_on`, the single runtime for tests that talk to Elasticsearch (see Live-Elasticsearch tests)
- `src/handlers/table.rs` — lays out a `SearchResponse` (hits or `aggregations`) as columns and rows
- `src/handlers/models.rs` — `Query`/`QueryMode` (shebang parsing) and the `ExecuteQueryResponse` conversions described above
- `src/utils/extractor.rs` — pulls `url`/`table`/`query` out of the raw RPC `params` JSON
- `src/utils/identifiers.rs`, `src/utils/pagination.rs` — SQL-identifier quoting and LIMIT/OFFSET helpers; currently unused by any handler (no DDL/pagination wired up yet), kept for when CRUD/DDL/pagination support is added
- `ui/` — separate pnpm/Vite package building a React UI extension (`ui/dist/index.js`) that Tabularis loads at runtime via the `ui_extensions` entry in `.tabularium`. It contributes a custom "Elasticsearch URL" field to the connection modal (slot `connection-modal.connection_content`) since this driver's "database" field is actually a full URL, not a plain name. React/`@tabularis/plugin-api` are Vite externals — the host provides them, don't bundle them.

### `.tabularium`

Declares plugin identity, capabilities (`readonly: true`, `no_connection_required: true`, no schemas/views/routines/FKs), the UI extension slot, and the pseudo data types shown in the Tabularis UI. Keep `capabilities` in sync with what's actually implemented in `rpc.rs` — e.g. don't flip `manage_tables`/`alter_column`/etc. to `true` without a matching DDL handler.

This repo is a fork (adds ES ≤ 6 support, aggregations, row limits). Its version is the upstream version it is based on plus `+thingnario.N` build metadata, kept the same in `Cargo.toml`, `.tabularium` and `manifest.json`. Tabularis compares plugin versions with semver, where `0.1.5+thingnario.1` sorts after `0.1.5` and before `0.1.6`: the registry's upstream `0.1.5` is not offered as an update over this build, while a newer upstream release still is. Installing that update replaces this build. Do not use a `-suffix`: it is a semver pre-release and sorts before `0.1.5`.

## Adding a new RPC method

1. Add the `match` arm in `rpc.rs`.
2. Implement the handler in the relevant `src/handlers/*.rs`, following the existing pattern: extract params via `utils::extractor`, build a `Client` via `Client::from_url`, map `Result<_, PluginError>` to `ok_response`/`error_response`.
3. If it touches a new ES response shape, add a `Deserialize` struct to `src/es/models.rs` and a method to `src/es/client.rs`.
4. Update `.tabularium` capabilities if the change affects what the UI should show.
