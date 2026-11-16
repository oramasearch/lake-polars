# lake-polars

**lake-polars, powered by Polars** — the web service behind lake's **`polars` service kind**:
analytics compute, not a database. A customer uploads datasets (CSV, JSONL or Parquet, stored as
Parquet) and runs Polars SQL over them through a small HTTP API with a bearer token. The Orama
operator runs a StatefulSet of `lake-polars` workers per service, each with its own volume, and lake
hands the customer the address and token.

It is built on the open-source [`polars`](https://github.com/pola-rs/polars) Rust crate (MIT). The
commercial Polars On-Prem product and its Kubernetes operator are **not** used.

```sh
make test            # cargo test --locked
make run             # serves ./local-data on 127.0.0.1:8080 with token local-dev-token
curl -s -H 'Authorization: Bearer local-dev-token' -H 'Content-Type: text/csv' \
  --data-binary $'id,city\n1,london\n2,paris\n' localhost:8080/v1/datasets/cities
curl -s -H 'Authorization: Bearer local-dev-token' \
  -d '{"sql":"SELECT city, count(*) AS n FROM cities GROUP BY city"}' localhost:8080/v1/query
```

| Route | Body | Answer |
|---|---|---|
| `POST /v1/datasets/<name>` | CSV, JSONL or Parquet (`Content-Type`, or `?format=`) | `201` (new) or `200` (replaced) `{name, rows, columns, bytes, replaced}` |
| `GET /v1/datasets` | — | `{datasets: [{name, rows, columns: [{name, type}], bytes}]}` |
| `DELETE /v1/datasets/<name>` | — | `{deleted}` |
| `POST /v1/query` | `{sql}` | `{columns, rows}` (at most 10,000 rows; `truncated` when cut) |
| `GET /healthz` | — | 200 when the data directory is readable and writable (no token) |

The whole API, configuration, limits, sandbox and errors: [`docs/api.md`](docs/api.md).

The image is `oramasearch/lake-polars` on Docker Hub. The operator side (StatefulSet, Service,
credential) is orama-operator `internal/controller/lake_service_polars.go`.
