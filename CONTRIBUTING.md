# Contributing to lake-polars

lake-polars, powered by Polars, is one Rust binary (`src/main.rs`) serving Parquet datasets and
Polars SQL over HTTP (`src/server.rs`; the store is `src/datasets.rs`, the SQL sandbox
`src/sandbox.rs`, JSON values `src/values.rs`). Its API contract is the `polars` row of lake's
`docs/control-plane/services.md` and the paragraph after the table; what is built is
[`docs/api.md`](docs/api.md). Change both together.

Thank you for helping. This document says how to build and test lake-polars, how a change gets in,
and the one thing every contributor does first: sign the Contributor License
Agreement.

## Before you start

- For a bug, open an issue with what you did, what you expected and what happened, and the
  version or commit.
- For a larger change (a new feature, a new dependency, a change to an interface), open an issue
  first so the approach can be agreed before you write it.
- **Security issues are never reported in a public issue.** See [SECURITY.md](SECURITY.md): write
  to **security@orama.com**.

## Build and test

You need Rust (the `rust-version` in `Cargo.toml` or later) and Cargo.

```sh
make test              # cargo test --locked
cargo build --locked --release
docker build -t lake-polars:local .
```

## Making a change

1. Fork the repository and branch from `dev`. `main` is the last release; it moves only when a
   release is cut.
2. Keep one change per pull request, with the tests that prove it. A change in behavior updates
   the documentation it touches in the same pull request.
3. Write commit messages in the [Conventional Commits](https://www.conventionalcommits.org/) form,
   for example `fix(api): reject an empty name`.
4. Sign the Contributor License Agreement before your first pull request is merged (next section).
5. Open the pull request against `dev`. Start its description with what changes, why, and how.

A maintainer reviews it. Pull requests are merged with a merge commit, so every commit you make
stays in the history.

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
  | `mimalloc` | 0.1.52 | MIT | the allocator (musl's own is slow under Polars' threads) |

- **The sandbox is load-bearing.** Polars SQL's table functions (`read_csv`, `read_parquet`,
  `read_json`, `read_ipc`) read any path the process can read. `src/sandbox.rs` allows exactly one
  query statement whose every FROM item is a plain table name, a subquery, a nested join or an
  `UNNEST`. On a Polars upgrade, re-read `polars-sql`'s `context.rs` (`execute_statement`,
  `get_table`) and `table_functions.rs` for new ways to reach files, and keep the tests in
  `tests/api.rs` (`file_reading_table_functions_are_refused`) green.
- **Never log** SQL, bodies, dataset contents, the query string, headers or the token.
- Images are built for `linux/amd64` on native amd64, never under emulation, and pushed with an
  immutable `<semver>` tag; the operator's default image (`lakeServices.polarsImage` in its chart)
  moves to the new tag after the push.
- **The image carries no GPL or LGPL code.** It is one statically linked musl binary on `scratch`
  (`Dockerfile`); the build fails if the binary has a dynamic dependency. Never move it to a base
  with a libc or a shell.
- **Licences are generated.** After any change to `Cargo.toml` or `Cargo.lock`, run
  `cargo fetch --locked && python3 tools/third-party-licenses.py` (it refuses a crate whose licence
  is not permissive) and `python3 tools/sbom.py`, and commit the results; `--check` on either
  verifies.

## Contributor License Agreement

lake-polars is owned by OramaSearch, Inc. Every contributor signs the
[Contributor License Agreement](CLA.md) (CLA) once, before their first pull request is merged. Under
it you assign the copyright in your contributions to OramaSearch, Inc., you keep a licence to use
your own contributions for any purpose, and you grant a patent licence for them. The project is
published under the MIT License (see [LICENSE](LICENSE)).

When you open your first pull request, a bot comments on it with a link to the agreement. Sign
there with your GitHub account; it covers all your later contributions to projects of OramaSearch,
Inc. If you contribute as part of your job, your employer signs too: write to
**opensource@orama.com** for the entity agreement.

## License headers

Every source file starts with the line `SPDX-License-Identifier: MIT`, in that file's comment
syntax, for example `// SPDX-License-Identifier: MIT`. Add it to a new file yourself, or run:

```sh
python3 tools/spdx-headers.py           # adds the line where it is missing
python3 tools/spdx-headers.py --check   # lists files without it
```

Code copied from another project keeps its own license header and is listed in
[THIRD_PARTY_LICENSES](THIRD_PARTY_LICENSES). Say so in the pull request, with where it came from
and its license; it must be compatible with the MIT License.

## Code of conduct

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md). Report unacceptable
behavior to **conduct@orama.com**.
