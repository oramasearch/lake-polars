# Contributing to lake-polars

lake-polars, powered by Polars, is one Rust binary (`src/main.rs`) serving Parquet datasets and
Polars SQL over HTTP (`src/server.rs`; the store is `src/datasets.rs`, the SQL sandbox
`src/sandbox.rs`, JSON values `src/values.rs`). Its API contract is the `polars` row of lake's
`docs/control-plane/services.md` and the paragraph after the table; what is built is
[`docs/api.md`](docs/api.md). Change both together.

## Build and test

```sh
cargo build --locked && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked
```

(`make lint test`). Debug builds are enough for tests.

## Rules for changes

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
- Documentation changes in the same pull request as the code.
- Branch from `dev` and open the pull request against `dev`; `main` holds the last release.
- Images are built for `linux/amd64` on native amd64, never under emulation, and pushed with an
  immutable `<semver>` tag; the operator's default image (`lakeServices.polarsImage` in its chart)
  moves to the new tag after the push.

## Security

Report a vulnerability privately, as [`SECURITY.md`](SECURITY.md) describes, never in a public issue.
