# lake-polars

**Upload datasets and run Polars SQL over them through a small, sandboxed HTTP API.**

lake-polars, powered by Polars, is the analytics service behind lake's `polars` service kind. You
upload CSV, JSON Lines or Parquet, it stores each upload as a Parquet file, and you query them with
Polars SQL behind a bearer token. It is compute, not a database: one Rust binary, one data
directory, no cluster of its own. Customer SQL is treated as untrusted code, and the queries that
could read files off the host are refused before they run.

Part of [Orama Lake](https://github.com/oramasearch/lake-platform), the sovereign AI stack.
MIT-licensed.

## What it does

- **Datasets**: upload CSV, JSON Lines or Parquet up to 256 MiB, stored as Parquet; replace, list
  and delete them. Datasets live on one worker's volume, with no replication. [Docs](docs/api.md#datasets)
- **Polars SQL**: joins, aggregates, CTEs and subqueries over every dataset, as lazy Parquet scans;
  at most 10,000 rows per answer. [Docs](docs/api.md#post-v1query)
- **The SQL sandbox**: exactly one query statement, and every FROM item a dataset name, a subquery,
  a join or an `UNNEST`; table functions such as `read_csv('/etc/passwd')` are refused. [Docs](docs/api.md#the-sql-sandbox)
- **Limits**: a query time limit, a concurrency limit shared by queries and uploads, and an upload
  cap tied to the memory limit; one query's own memory is not bounded below the container limit.
  [Docs](docs/api.md#limits)
- **Logging without data**: one JSON line per request, never the SQL, a body, a header or the
  token. [Docs](docs/api.md#lifecycle-and-logging)

The whole API, configuration and every error code are in [`docs/api.md`](docs/api.md); the
security model is in [`docs/security.md`](docs/security.md).

## Quickstart

```sh
git clone https://github.com/oramasearch/lake-polars.git
cd lake-polars
make run             # serves ./local-data on 127.0.0.1:8080 with the example token local-dev-token
```

In a second terminal:

```sh
curl -s -H 'Authorization: Bearer local-dev-token' -H 'Content-Type: text/csv' \
  --data-binary $'id,city\n1,london\n2,paris\n3,london\n' localhost:8080/v1/datasets/cities
curl -s -H 'Authorization: Bearer local-dev-token' \
  -d '{"sql":"SELECT city, count(*) AS n FROM cities GROUP BY city ORDER BY city"}' localhost:8080/v1/query
```

The second call answers `{"columns":["city","n"],"rows":[["london",2],["paris",1]]}`.
`local-dev-token` is an example: set your own `LAKE_POLARS_TOKEN` anywhere but your own machine.

### Build from source

Rust 1.85 or later, a C compiler (for the bundled zstd and lz4), and `make`:

```sh
make build           # cargo build --locked
make lint test       # rustfmt, clippy (warnings are errors) and every test
docker build -t lake-polars:local .   # a static binary on an empty (scratch) image
```

The tests need no network and no services: they run the API in memory over temporary directories.

## How it fits the stack

lake hands a customer the address and token of their `polars` service; the operator runs
lake-polars as one StatefulSet per service, with its own volume, and mounts the token from a Secret.
Nothing calls out from lake-polars: it only answers HTTP.

| Piece | What it does | Relation to this repo |
|---|---|---|
| [liquid](https://github.com/oramasearch/liquid) | Version control, builds, packages, deploys | builds and ships the image |
| [lake](https://github.com/oramasearch/lake) | The AI workspace | defines the `polars` service kind and its contract; its users call this API |
| [zetta](https://github.com/oramasearch/zetta) | The engine | none |
| [orama-operator](https://github.com/oramasearch/orama-operator) | Runs the stack on Kubernetes | runs this service, its volume and its token |

## Known limits

- **Datasets are per worker.** With more than one instance, each worker has its own datasets and
  the Service spreads requests across them: run one instance until shared storage exists.
- **One copy.** A worker's datasets live on one volume, with no replication and no backup.
- **One query's memory is not bounded** below the container's limit: a large join or sort can get
  the process killed (the datasets survive). See [Memory](docs/api.md#limits).
- **A running query cannot be cancelled.** Past its time limit the caller gets `504`, and the query
  finishes in the background, still holding its slot.
- **Plain HTTP.** TLS is the platform's job (the mesh or ingress in front of the Service).

## Documentation

- [`docs/api.md`](docs/api.md): the HTTP API, configuration, limits, the SQL sandbox, errors,
  build and local run.
- [`docs/security.md`](docs/security.md): the trust boundaries, what is defended and how, and the
  security review record.
- [`docs/README.md`](docs/README.md): the docs index.

## Community

- Questions and help: [GitHub Discussions](https://github.com/oramasearch/lake-polars/discussions)
- Bugs and requests: [issues](https://github.com/oramasearch/lake-polars/issues)
- Security reports: privately, per [SECURITY.md](SECURITY.md). Please don't open a public issue.

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) for how to build, test and open
a pull request (against `dev`). Every contributor signs the
[Contributor License Agreement](CLA.md) once, before their first pull request is merged.

Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).

## License

MIT. See [LICENSE](LICENSE) and [COPYRIGHT](COPYRIGHT). Third-party components keep their own
licenses, listed in [THIRD_PARTY_LICENSES](THIRD_PARTY_LICENSES).

"Polars" is the name of the open-source project lake-polars is built on; lake-polars is not made or
run by the Polars project or its company. "Orama Lake", "liquid", "lake", "zetta" and their logos
are trademarks of OramaSearch, Inc. The
[trademark policy](https://github.com/oramasearch/lake-platform/blob/main/TRADEMARKS.md) says how
you can use them, including in a fork.
