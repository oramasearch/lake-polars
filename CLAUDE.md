# CLAUDE.md — lake-polars

Org-wide conventions are in [`oramasearch/orama-bridge`](https://github.com/oramasearch/orama-bridge)
`CLAUDE.md` and apply in full: no AI attribution, merge commits never squash, commit means push,
private by default, documentation in the same change.

- **What it is:** one Rust binary (`src/main.rs`) serving Parquet datasets and Polars SQL over HTTP
  (`src/server.rs`; the store is `src/datasets.rs`, the SQL sandbox `src/sandbox.rs`, JSON values
  `src/values.rs`). The API contract is lake `docs/control-plane/services.md` (the `polars` row and
  the paragraph after the table); what is built is [`docs/api.md`](docs/api.md). Change both
  together.
- **Branding:** "lake-polars, powered by Polars". Never "Polars Cloud" or anything implying the
  vendor runs it. The commercial Polars On-Prem product and its operator are not used.
- **Crates** (exact versions pinned in `Cargo.toml`, `Cargo.lock` committed; every transitive
  licence is permissive — MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0, BSL-1.0):

  | Crate | Version | Licence | Why |
  |---|---|---|---|
  | `polars` (features `lazy`, `sql`, `parquet`, `csv`, `json`, `dtype-full`, `temporal`, `strings`, `timezones`) | 0.55.2 | MIT | the engine and its SQL |
  | `sqlparser` (feature `visitor`) | 0.62.0 | Apache-2.0 | the sandbox; **must be the release `polars-sql` depends on** (check `Cargo.lock` on every Polars bump) |
  | `axum` | 0.8.9 | MIT | HTTP server |
  | `tokio` | 1.53.1 | MIT | runtime |
  | `http-body-util` | 0.1.3 | MIT | the body cap |
  | `serde`, `serde_json` | 1.0.228, 1.0.151 | MIT OR Apache-2.0 | JSON |
  | `sha2` | 0.10.9 | MIT OR Apache-2.0 | token hashing |
  | `subtle` | 2.6.1 | BSD-3-Clause | constant-time compare |

- **The sandbox is load-bearing.** Polars SQL's table functions (`read_csv`, `read_parquet`,
  `read_json`, `read_ipc`) read any path the process can read. `src/sandbox.rs` allows exactly one
  query statement whose every FROM item is a plain table name, a subquery, a nested join or an
  `UNNEST`. On a Polars upgrade, re-read `polars-sql`'s `context.rs` (`execute_statement`,
  `get_table`) and `table_functions.rs` for new ways to reach files, and keep the tests in
  `tests/api.rs` (`file_reading_table_functions_are_refused`) green.
- **Never log** SQL, bodies, dataset contents, the query string, headers or the token.
- **Build and verify:** `cargo build --locked && cargo clippy --locked --all-targets -- -D warnings
  && cargo test --locked` (`make lint test`). Debug builds are enough for tests.
- **Release:** build `linux/amd64` on the cluster BuildKit (liquid-bridge `.claude/skills/liquid-dev`)
  — never under emulation — create the Docker Hub repository **private** before the first push,
  push `oramasearch/lake-polars:<semver>` (an immutable tag), confirm an anonymous
  `https://hub.docker.com/v2/repositories/oramasearch/lake-polars/` still answers 404, then bump the
  operator's default image (`chart/values.yaml` `lakeServices.polarsImage`, `cmd/main.go`
  `--lake-polars-image`).
