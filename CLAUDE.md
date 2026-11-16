# CLAUDE.md — lake-polars

- **What it is:** one Rust binary (`src/main.rs`) serving Parquet datasets and Polars SQL over HTTP
  (`src/server.rs`; the store is `src/datasets.rs`, the SQL sandbox `src/sandbox.rs`, JSON values
  `src/values.rs`). The API contract is lake's `docs/control-plane/services.md` (the `polars` row and
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
  | `mimalloc` | 0.1.52 | MIT | the allocator (musl's own is slow under Polars' threads) |

- **The sandbox is load-bearing.** Polars SQL's table functions (`read_csv`, `read_parquet`,
  `read_json`, `read_ipc`) read any path the process can read. `src/sandbox.rs` allows exactly one
  query statement whose every FROM item is a plain table name, a subquery, a nested join or an
  `UNNEST`. On a Polars upgrade, re-read `polars-sql`'s `context.rs` (`execute_statement`,
  `get_table`) and `table_functions.rs` for new ways to reach files, and keep the tests in
  `tests/api.rs` (`file_reading_table_functions_are_refused`) green.
- **Security model:** [`docs/security.md`](docs/security.md) lists every attack the service
  defends against and where. A change that adds an entry point, a SQL function that allocates from
  a literal, or a new way to hold a connection updates that table and adds a test.
- **Never log** SQL, bodies, dataset contents, the query string, headers or the token.
- **Build and verify:** `cargo build --locked && cargo clippy --locked --all-targets -- -D warnings
  && cargo test --locked` (`make lint test`). Debug builds are enough for tests.
- **Images** are built for `linux/amd64` natively, never under emulation, and tagged with an
  immutable semantic version. A new image version is adopted by bumping the operator's default
  (`chart/values.yaml` `lakeServices.polarsImage`, `cmd/main.go` `--lake-polars-image`).
- **The image carries no GPL or LGPL code.** It is one statically linked musl binary on `scratch`
  (`Dockerfile`); the build fails if the binary has a dynamic dependency. Never move it to a base
  with a libc or a shell.
- **Licences are generated.** After any change to `Cargo.toml` or `Cargo.lock`, run
  `cargo fetch --locked && python3 tools/third-party-licenses.py` (it refuses a crate whose licence
  is not permissive) and `python3 tools/sbom.py`, and commit the results; `--check` on either
  verifies.

## Rules

- **Branches:** branch from `dev`, and open every pull request against `dev`. `main` holds the last
  release. Pull requests are merged with merge commits, never squashed.
- **Documentation in the same change:** `README.md` and the pages under `docs/` change in the same
  commit as the code they describe.
- **No AI attribution** in commits, pull requests, tags or release notes: no `Co-Authored-By`
  trailers for an assistant, no "Generated with" lines.
- **Never write the word "seam"**; name the mechanism (interface, extension point, injected
  dependency, boundary).
- **No time or size estimates**; report what was done.
- Report vulnerabilities privately, as `SECURITY.md` describes, never in a public issue.
