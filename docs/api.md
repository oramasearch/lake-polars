# lake-polars

`lake-polars` — **lake-polars, powered by Polars** — is the small web service behind lake's
**`polars` service kind**: analytics compute over uploaded datasets, not a database. There is no
upstream Kubernetes operator we would use (the commercial Polars On-Prem product and its operator
are not used: the liquid-bridge `/lake-service` skill excludes it on its terms), so the Orama operator runs it itself, one StatefulSet
per service (orama-operator `docs/lake/services.md`; liquid-bridge decision 47, amended; lake#195).
The contract is the `polars` row of lake's `docs/control-plane/services.md` and the paragraph after
its table; this page is the whole API as built.

| | |
|---|---|
| Source | this repository: [`src/main.rs`](../src/main.rs) (configuration, startup, shutdown), [`src/server.rs`](../src/server.rs) (the HTTP API), [`src/datasets.rs`](../src/datasets.rs) (the store), [`src/sandbox.rs`](../src/sandbox.rs) (the SQL sandbox), [`src/values.rs`](../src/values.rs) (JSON values) |
| Image | `oramasearch/lake-polars:<tag>` (a **private** Docker Hub repository), built from [`Dockerfile`](../Dockerfile) for `linux/amd64` |
| Engine | the open-source [`polars`](https://github.com/pola-rs/polars) crate 0.55.2, **MIT**, with its SQL front end (`polars-sql`) |
| HTTP | `axum` 0.8.9 on `tokio` 1.53.1, both **MIT** |
| Runtime | `gcr.io/distroless/cc-debian12:nonroot` (glibc, no shell), user `65532:65532`, one binary |

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `LAKE_POLARS_TOKEN` | — (required) | the bearer token; the operator mounts it from Secret `<name>-credentials`, key `token` |
| `LAKE_POLARS_DIR` | `/data` | the dataset directory; it must exist and be writable (the service exits otherwise) |
| `LAKE_POLARS_ADDR` | `0.0.0.0:8080` | the listen address |
| `LAKE_POLARS_MAX_BODY_BYTES` | `268435456` (256 MiB) | the largest upload |
| `LAKE_POLARS_QUERY_TIMEOUT_SECONDS` | `30` | the query time limit |
| `LAKE_POLARS_MAX_CONCURRENT` | `2` | how many queries and uploads run at once |
| `LAKE_POLARS_MEMORY_LIMIT` | unset | the container memory limit in bytes (the operator passes `limits.memory`); see *Memory* below |
| `LAKE_POLARS_THREADS` | unset (every CPU the container may use) | Polars' thread pool size; set as `POLARS_MAX_THREADS` before Polars starts |

At startup the service checks that the directory can be listed and written, removes upload temp
files a crash left behind (`.upload-*.tmp`), and exits non-zero if the token is empty, a number is
malformed or the directory is unusable.

## Datasets

A dataset is one Parquet file, `<LAKE_POLARS_DIR>/<name>.parquet`. Its name matches
**`^[a-z][a-z0-9_]{0,62}$`** — a lowercase SQL identifier, so it is also its table name in queries
and can never name a path outside the directory. Other files in the directory are ignored.

Datasets are **per worker**: with `instances` above 1, each worker has its own volume and its own
datasets, and the Service spreads requests across workers, so an upload lands on one worker only.
Until shared storage exists (lake#196), **1 is the only useful `instances` value**.

## API

Every route but `GET /healthz` needs **`Authorization: Bearer <token>`**. The presented and the
configured token are both hashed with SHA-256 and compared in constant time; a missing, malformed
or wrong token is `401` (with `WWW-Authenticate: Bearer`), before the route or method is looked at.
Every answer is JSON with `Cache-Control: no-store`.

### `POST /v1/datasets/<name>`

The body is the data. Its format is `?format=csv|jsonl|ndjson|parquet` when given, else the
`Content-Type` (parameters such as `charset` are ignored):

| Format | `Content-Type` |
|---|---|
| CSV (a header row, comma-separated; types inferred from the first 10,000 rows; dates parsed when they look like dates) | `text/csv`, `application/csv` |
| JSON Lines (one object per line; types inferred from the first 10,000 lines) | `application/x-ndjson`, `application/ndjson`, `application/jsonl`, `application/jsonlines`, `application/x-jsonlines` |
| Parquet | `application/vnd.apache.parquet`, `application/x-parquet`, `application/parquet` |

Anything else (including `application/octet-stream` without `?format=`) is `415
unsupported_format`. The data is parsed, written as Parquet to a temp file in the same directory,
synced and **renamed into place**, so a query sees the old dataset or the new one, never half of
one, and a failed upload leaves nothing behind.

**An upload to an existing name replaces that dataset.** The answer is `201` for a new dataset and
`200` for a replacement:

```json
{"name": "orders", "rows": 3, "bytes": 1234, "replaced": false,
 "columns": [{"name": "order_id", "type": "i64"}, {"name": "amount", "type": "f64"}]}
```

`type` is Polars' name for the column type (`i64`, `f64`, `str`, `bool`, `date`,
`datetime[μs]`, …). `bytes` is the Parquet file's size.

Uploads have no time limit (a timed-out upload would still land a moment later), but they share the
`LAKE_POLARS_MAX_CONCURRENT` slots with queries.

### `GET /v1/datasets`

`{"datasets": [...]}`, one entry per dataset in name order, in the shape above without `replaced`.
Row counts and schemas come from each file's Parquet footer; no data is scanned.

### `DELETE /v1/datasets/<name>`

Removes the dataset: `200` `{"deleted": "<name>"}`, or `404 not_found` when there is none. A query
already running over it finishes on the file it opened.

### `POST /v1/query`

`{"sql": "..."}` (strict JSON: unknown fields are refused; at most 1 MiB) runs **Polars SQL** with
every dataset registered as a table of its name, each a lazy Parquet scan (only the columns and row
groups the plan needs are read). It answers `200`:

```json
{"columns": ["name", "orders", "total"],
 "rows": [["grace", 1, 10.25], ["ada", 2, 6.5]],
 "truncated": true}
```

At most **10,000 rows** are returned: the plan is limited to 10,001 rows, and when there were more
than 10,000 the answer stops there and carries `"truncated": true` (the key is absent otherwise).

Values come back as JSON types:

| Polars | JSON |
|---|---|
| null | `null` |
| Boolean | `true` / `false` |
| Int8–Int64, UInt8–UInt64 | a JSON integer (exact to 64 bits; a JavaScript client loses precision above 2^53) |
| Int128, UInt128, Decimal | a string, so no digit is lost |
| Float32, Float64 | a JSON number; NaN and infinities as the strings `"NaN"`, `"Infinity"`, `"-Infinity"` |
| String, Categorical, Enum | a string |
| Date | `"2026-09-26"` |
| Datetime | ISO 8601, `"2026-09-26T10:11:12"` (fractional seconds when there are any; the zone when the column has one) |
| Time | `"10:11:12"` |
| Duration, List, Array, Struct | Polars' own text form, as a string |
| Binary | `{"$base64": "<standard base64>"}` |

#### The SQL sandbox

Customer SQL is treated as untrusted code. Polars SQL has table functions — `read_csv`,
`read_parquet`, `read_json`, `read_ipc` — that read **any path the process can read**, and
statements (`CREATE TABLE`, `DROP`, `DELETE`, …) that change the session. Before a query reaches
Polars, lake-polars parses it with the same parser, dialect and options Polars uses (`sqlparser`
0.62, the generic dialect) and refuses it with `400 forbidden_sql` unless:

- it is **exactly one statement**, and that statement is a **query** (`SELECT`, `WITH`, `UNION`, …);
- **every** FROM item in it — at any depth: joins, subqueries, CTEs, `IN (SELECT …)`, scalar
  subqueries — is a plain table name **without arguments**, a subquery, a parenthesized join, or an
  `UNNEST` of literal arrays.

It is an allowlist, so a table function or FROM form added in a later Polars release is refused
until it is reviewed. Table names resolve only to the registered datasets; a name that is not a
dataset is `400 sql_error` (`relation '<name>' was not found`).

### `GET /healthz`

No token. `200` `{"status": "ok"}` when the data directory can be listed and a probe file created in
it and removed; otherwise `503` `{"error": {"code": "unavailable", ...}}`. `HEAD` is accepted too.
The operator's probes call it.

### Errors

Every error is `{"error": {"code": "...", "message": "..."}}`:

| Status | `code` | When |
|---|---|---|
| 400 | `invalid_request` | the query body is not `{"sql": "..."}`, `sql` is empty, or an upload body is empty |
| 400 | `invalid_name` | the dataset name does not match `^[a-z][a-z0-9_]{0,62}$` |
| 400 | `invalid_data` | the upload does not parse as its format (the message is Polars') |
| 400 | `sql_error` | the SQL does not parse, or Polars refused or failed it (unknown table or column, type error, …); the message is Polars' and may show the query plan |
| 400 | `forbidden_sql` | the sandbox refused the SQL (above) |
| 401 | `unauthorized` | no or a wrong bearer token |
| 404 | `not_found` | an unknown route (with a valid token), or `DELETE` of a dataset that does not exist |
| 405 | `method_not_allowed` | a route called with the wrong method |
| 413 | `body_too_large` | an upload over the upload cap (below), or a query body over 1 MiB |
| 415 | `unsupported_format` | no usable `?format=` or `Content-Type` on an upload |
| 500 | `internal` | an unexpected failure (including a Polars panic, which is caught) |
| 503 | `unavailable` | `/healthz`: the directory is not usable |
| 504 | `timeout` | the query ran past its time limit |
| 507 | `disk_full` | the volume is full |

## Limits

**Time.** Each query has `LAKE_POLARS_QUERY_TIMEOUT_SECONDS` (30 s), counted from when the request
is read, including any wait for a slot. Polars cannot be interrupted mid-query: when the time runs
out the caller gets `504 timeout` at once, and a query that had started **finishes in the
background, still holding its slot**, and its result is discarded. Because it keeps its slot,
timed-out queries can never pile up beyond `LAKE_POLARS_MAX_CONCURRENT`; new requests wait (inside
their own time limit) for a slot instead.

**Concurrency.** At most `LAKE_POLARS_MAX_CONCURRENT` (2) queries and uploads run at once; each runs
on the blocking thread pool, and Polars parallelizes each one over its own thread pool
(`LAKE_POLARS_THREADS`).

**Memory.** The container's memory limit is the hard stop: a query that needs more is killed with
the process (the pod restarts; datasets on the volume survive). The guard in front of it, when
`LAKE_POLARS_MEMORY_LIMIT` is set:

- **does** lower the upload cap to a quarter of the memory limit (a parsed upload and the Parquet
  writer take several times the raw bytes), so an upload that would certainly not fit is refused
  with `413` before it is read;
- **does** bound how many queries and uploads hold memory at once (the concurrency slots);
- **does not** bound one query's own memory: a join or sort over large datasets can still exceed the
  limit. Keep queries selective (filters and projections are pushed into the Parquet scans) and size
  `limits.memory` for the largest query expected.

**Bodies.** Uploads up to `LAKE_POLARS_MAX_BODY_BYTES` (256 MiB, lower under the memory guard);
query bodies up to 1 MiB. The whole upload is held in memory while it is parsed.

## Lifecycle and logging

- On `SIGTERM` (or `SIGINT`) the server stops accepting connections and finishes in-flight requests;
  work still running afterwards (a query past its timeout, an upload) gets up to 30 s more. An upload
  cut off by the exit leaves only a temp file, removed at the next start.
- One JSON line per request on stdout: `method`, `path`, `status`, `bytes`, `request_bytes`,
  `duration_ms`. The SQL, bodies, dataset contents, the query string, headers and the token are
  **never** logged.

## Build

```sh
docker buildx build --platform linux/amd64 -t oramasearch/lake-polars:<tag> .
# or: make docker-build IMG=oramasearch/lake-polars:<tag>
```

Build on native amd64 (the cluster BuildKit), never under emulation. The builder is
`rust:1.95-bookworm` and the runtime `gcr.io/distroless/cc-debian12:nonroot`, both digest-pinned,
both Debian 12 glibc. The chart's default is `lakeServices.polarsImage:
oramasearch/lake-polars:0.1.0`, so that tag must be published (**private** Docker Hub: create the
repository private before the first push) before a Polars service can start; a Lake overrides it
with `spec.services.polarsImage`.

## Run locally

```sh
cargo test --locked
mkdir -p /tmp/lake-polars
LAKE_POLARS_TOKEN=dev LAKE_POLARS_DIR=/tmp/lake-polars LAKE_POLARS_ADDR=127.0.0.1:8080 cargo run

curl -s localhost:8080/healthz
curl -s -H 'Authorization: Bearer dev' -H 'Content-Type: application/x-ndjson' \
  --data-binary $'{"id":1,"amount":2.5}\n{"id":2,"amount":4}\n' localhost:8080/v1/datasets/orders
curl -s -H 'Authorization: Bearer dev' localhost:8080/v1/datasets
curl -s -H 'Authorization: Bearer dev' -d '{"sql":"SELECT sum(amount) AS total FROM orders"}' localhost:8080/v1/query
```

The tests (`tests/api.rs` over the router in memory and a temporary directory, plus unit tests in
`src/`) cover: uploading CSV, JSONL and Parquet (by `Content-Type` and by `?format=`), listing,
replacing, a join with an aggregate, value types, the row cap and `truncated`, the token (missing,
wrong, prefix, suffix, another scheme; `/healthz` without one), `/healthz` failing without its
directory, delete, invalid names, formats and bodies, the body cap and the memory guard's upload
cap, the sandbox refusing `read_csv('/etc/passwd')`, `read_parquet`, `read_json`, `read_ipc` at any
depth and every non-query statement, the query timeout, and that the request log never carries the
SQL, data or a token.

## Known limits

- **Datasets are per worker** until shared storage (lake#196): use `instances: 1`.
- **One copy.** Each worker's datasets live on one `ReadWriteOnce` volume, with no replication and
  no backup yet: a lost volume loses them.
- One query's memory is not bounded below the container limit (see *Memory*).
- A query cannot be cancelled once it has started; see *Time*.
