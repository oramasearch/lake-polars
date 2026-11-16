# Security model

How lake-polars is meant to be attacked, what stops each attack, and what is left to the platform.
The API itself is in [`api.md`](api.md); to report a vulnerability, see
[`SECURITY.md`](../SECURITY.md).

## Trust boundaries

| Who | Can | Trusted with |
|---|---|---|
| Anyone who can reach the port | open connections, send requests, call `GET /healthz` | nothing |
| A holder of the bearer token | upload, list, delete and query this service's datasets | this service's datasets, and nothing else |
| The operator | set the environment, mount the volume and the token | everything (it runs the process) |

The token holder is not trusted beyond its datasets. It is often an app or an agent acting for a
customer, and the SQL or files it relays may come from untrusted content. So **SQL text and
uploaded files are hostile input**: they must not read or write anything outside the dataset
directory, run anything but one query, reveal the token or the environment, or take the process
down beyond the documented limits.

One process serves one service. Its datasets are on its own volume, so there is no other tenant in
the process to reach.

## What is defended, and how

| Attack | Defence | Where |
|---|---|---|
| Read a host file through SQL (`read_csv('/etc/passwd')`, `read_parquet`, …) | the sandbox: one query statement, every FROM item a dataset name, subquery, join or `UNNEST` (an allowlist) | `src/sandbox.rs` |
| Change the session (`CREATE TABLE`, `DROP`, `DELETE`, `SHOW`, `EXPLAIN`) | the same sandbox: only queries | `src/sandbox.rs` |
| Abort the process with a deep expression (`1+1+1+…`) | at most 1,000 operators, counted on tokens before parsing; each query on a 64 MiB stack | `src/sandbox.rs`, `src/server.rs` |
| Allocate without bound from one line (`lpad('a', 1e12)`) | `lpad` / `rpad` lengths are literals ≤ 65,536 | `src/sandbox.rs` |
| Path traversal through a dataset name | names match `^[a-z][a-z0-9_]{0,62}$`; symlinks in the directory are ignored | `src/datasets.rs` |
| Guess or time the token | SHA-256 of both tokens, constant-time compare; every route but `/healthz` | `src/server.rs` |
| Exhaust memory with uploads | the upload cap (a quarter of the memory limit); an upload takes its concurrency slot before its body is read; the Parquet footer's decoded size is checked | `src/server.rs`, `src/datasets.rs` |
| Hold slots or connections open (slow clients) | header read timeout (30 s), at most 512 connections, a deadline on every body | `src/server.rs` |
| Pile up timed-out queries | a timed-out query keeps its slot until it ends, so no more than `LAKE_POLARS_MAX_CONCURRENT` run | `src/server.rs` |
| Get data or the token into logs | the request log holds method, path, status, sizes and duration only; panics log their location only | `src/server.rs`, `src/main.rs` |
| Wedge the service when the volume is full | crash leftovers are swept before the startup probe; the probe writes an empty file, so a full volume stays healthy and `DELETE` can free space | `src/main.rs`, `src/server.rs` |
| A compromised image base | the image is `scratch` with one static binary, user 65532; the builder is digest-pinned and not shipped | `Dockerfile` |

Every defence has a test in `tests/api.rs` or in the module's unit tests.

## Left to the platform

- **TLS.** The service speaks plain HTTP; the mesh or ingress in front of it terminates TLS.
- **Who can reach the port.** A NetworkPolicy should admit only lake and the customer's own
  workloads.
- **The token.** The operator generates it and mounts it from a Secret; lake-polars does not check
  its strength.
- **One query's memory.** A large join or sort can still exceed the container's memory limit; the
  process is then killed and restarted, and the datasets survive.

## Review record

| Date | Commit | Method | Result |
|---|---|---|---|
| 2026-11-15 | the pull request that adds this page | two independent static reviews of every source file and the Dockerfile, modelling an unauthenticated client, a hostile token holder and a malicious upstream, each finding traced through the code with `file:line` hops and reproduced by a test before it was fixed; the sandbox checked against `polars-sql` 0.55.2's statement and table resolution | no sandbox bypass. Fixed: uploads buffered before taking a concurrency slot; deep operator chains aborting the process (stack overflow); `lpad`/`rpad` allocating without bound; no header read timeout or connection limit; a full volume wedging startup and the health check; racing health probes; Parquet decompression bombs; panic messages reaching the log. Open: none |

On a Polars upgrade, re-read `polars-sql`'s `context.rs` (`execute_statement`, `get_table`),
`table_functions.rs` and `functions.rs` for new ways to reach files or to allocate from a literal,
and keep the sandbox tests green.
