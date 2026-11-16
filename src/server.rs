// SPDX-License-Identifier: MIT

//! The HTTP API: datasets, queries and health, behind a bearer token. `docs/api.md` is the
//! reference; this file is the implementation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{Path as UrlPath, Query, Request, State as AxumState};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use http_body_util::{BodyExt, LengthLimitError, Limited};
use polars::prelude::IdxSize;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::datasets::{self, Format, StoreError};
use crate::sandbox::{self, Refusal};
use crate::values;

/// The rows one query answers at most.
pub const DEFAULT_MAX_ROWS: usize = 10_000;
/// The largest upload: 256 MiB.
pub const DEFAULT_MAX_BODY_BYTES: usize = 256 << 20;
/// The largest `POST /v1/query` body: 1 MiB of JSON is a very long query.
pub const QUERY_BODY_BYTES: usize = 1 << 20;
/// How long a query may run before the caller gets `504 timeout`.
pub const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(30);
/// How many queries and uploads run at once; the rest wait their turn (inside their timeout).
pub const DEFAULT_MAX_CONCURRENT: usize = 2;
/// How long an upload may take to arrive once it holds its permit, before `408 timeout`.
pub const DEFAULT_UPLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// How long a client may take to send a request's headers before the connection is closed.
pub const DEFAULT_HEADER_TIMEOUT: Duration = Duration::from_secs(30);
/// How many connections are open at once; one more is closed as soon as it is accepted.
pub const DEFAULT_MAX_CONNECTIONS: usize = 512;
/// The stack each query and upload runs on. Planning a query recurses over its expression tree;
/// the sandbox bounds that tree's size, and this leaves the recursion ample room.
pub const WORK_STACK_BYTES: usize = 64 << 20;
/// Under a memory limit, an upload may be at most this fraction of it: a parsed frame and the
/// Parquet writer's buffers take several times the raw bytes.
pub const UPLOAD_MEMORY_DIVISOR: u64 = 4;

/// Where request log lines go (one JSON object per call, without a trailing newline).
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

/// The service's configuration.
#[derive(Clone)]
pub struct Config {
    /// The data directory; it must exist and be writable.
    pub dir: PathBuf,
    /// The bearer token every route but `/healthz` requires.
    pub token: String,
    pub max_rows: usize,
    pub max_body_bytes: usize,
    pub query_timeout: Duration,
    /// How long an upload body may take to arrive (it holds a permit while it does).
    pub upload_timeout: Duration,
    pub max_concurrent: usize,
    /// How long a client may take to send a request's headers.
    pub header_timeout: Duration,
    /// How many connections may be open at once.
    pub max_connections: usize,
    /// The container memory limit in bytes, when known (the memory guard).
    pub memory_limit: Option<u64>,
    pub log: LogSink,
}

impl Config {
    /// A configuration with every default, logging nowhere.
    pub fn new(dir: impl Into<PathBuf>, token: impl Into<String>) -> Config {
        Config {
            dir: dir.into(),
            token: token.into(),
            max_rows: DEFAULT_MAX_ROWS,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            query_timeout: DEFAULT_QUERY_TIMEOUT,
            upload_timeout: DEFAULT_UPLOAD_TIMEOUT,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            header_timeout: DEFAULT_HEADER_TIMEOUT,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            memory_limit: None,
            log: Arc::new(|_| {}),
        }
    }

    /// The largest upload accepted: the body cap, lowered to a quarter of the memory limit.
    /// The most bytes an upload may decode to (its Parquet footer's figures), under a memory limit.
    pub fn decoded_cap(&self) -> Option<u64> {
        self.memory_limit.map(|limit| limit / 2)
    }

    pub fn upload_cap(&self) -> usize {
        match self.memory_limit {
            Some(limit) => self
                .max_body_bytes
                .min(usize::try_from(limit / UPLOAD_MEMORY_DIVISOR).unwrap_or(usize::MAX)),
            None => self.max_body_bytes,
        }
    }
}

struct Inner {
    config: Config,
    token_hash: [u8; 32],
    work: Arc<Semaphore>,
}

/// Shared state of the router.
#[derive(Clone)]
pub struct State(Arc<Inner>);

impl State {
    pub fn new(config: Config) -> State {
        let token_hash = Sha256::digest(config.token.as_bytes()).into();
        let work = Arc::new(Semaphore::new(config.max_concurrent.max(1)));
        State(Arc::new(Inner {
            config,
            token_hash,
            work,
        }))
    }

    fn dir(&self) -> &Path {
        &self.0.config.dir
    }
}

/// The one error shape: `{"error": {"code", "message"}}`.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiError {
        ApiError {
            status,
            code,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({"error": {"code": self.code, "message": self.message}});
        let mut response = (self.status, Json(body)).into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"lake-polars\""),
            );
        }
        response
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> ApiError {
        match e {
            StoreError::BadData(m) => ApiError::new(StatusCode::BAD_REQUEST, "invalid_data", m),
            StoreError::TooLarge(m) => {
                ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large", m)
            }
            StoreError::NotFound => {
                ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such dataset")
            }
            StoreError::DiskFull => ApiError::new(
                StatusCode::INSUFFICIENT_STORAGE,
                "disk_full",
                "the volume is full",
            ),
            StoreError::Internal(m) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", m)
            }
        }
    }
}

/// The whole API, with the token check and the request log.
pub fn router(state: State) -> Router {
    Router::new()
        .route("/healthz", get(health).fallback(wrong_method))
        .route("/v1/datasets", get(list).fallback(wrong_method))
        .route(
            "/v1/datasets/{name}",
            post(upload).delete(remove).fallback(wrong_method),
        )
        .route("/v1/query", post(query).fallback(wrong_method))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), authorize))
        .layer(middleware::from_fn_with_state(state.clone(), log_request))
        .layer(middleware::map_response(no_store))
        .with_state(state)
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such route")
}

async fn wrong_method() -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "this route does not take that method",
    )
}

/// `Authorization: Bearer <token>` on everything but `/healthz`. Both tokens are hashed with
/// SHA-256 first, so the constant-time comparison does not depend on the presented length.
async fn authorize(AxumState(state): AxumState<State>, request: Request, next: Next) -> Response {
    if request.uri().path() == "/healthz" {
        return next.run(request).await;
    }
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let hash: [u8; 32] = Sha256::digest(presented.as_bytes()).into();
    if presented.is_empty() || !bool::from(hash.ct_eq(&state.0.token_hash)) {
        return ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "a valid bearer token is required",
        )
        .into_response();
    }
    next.run(request).await
}

/// One JSON line per request: method, path, status, response and request sizes, duration. Never a
/// header, the query string, a body, the SQL or data, so neither the token nor customer data can
/// reach the log.
async fn log_request(AxumState(state): AxumState<State>, request: Request, next: Next) -> Response {
    let start = Instant::now();
    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let request_bytes = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let response = next.run(request).await;
    let line = json!({
        "level": "INFO",
        "msg": "request",
        "method": method,
        "path": path,
        "status": response.status().as_u16(),
        "bytes": response.body().size_hint().exact(),
        "request_bytes": request_bytes,
        "duration_ms": start.elapsed().as_secs_f64() * 1000.0,
    });
    (state.0.config.log)(&line.to_string());
    response
}

/// Reads a body of at most `cap` bytes.
async fn read_body(headers: &HeaderMap, body: Body, cap: usize) -> Result<Bytes, ApiError> {
    let too_large = || {
        ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body_too_large",
            format!("the body is over {cap} bytes"),
        )
    };
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > cap as u64) {
        return Err(too_large());
    }
    match Limited::new(body, cap).collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(e) if e.downcast_ref::<LengthLimitError>().is_some() => Err(too_large()),
        Err(_) => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "cannot read the body",
        )),
    }
}

/// Runs blocking Polars or disk work on the blocking pool, holding one of the `max_concurrent`
/// permits for as long as the work runs. With a timeout, the caller is answered `504` when it runs
/// out, whether the work was still waiting for a permit (it then never starts) or running (Polars
/// cannot be interrupted: the work finishes in the background, still holding its permit, so timed
/// out queries cannot pile up beyond the permit count).
async fn run_blocking<T, F>(
    state: &State,
    timeout: Option<Duration>,
    work: F,
) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
{
    let state = state.clone();
    let task = async move {
        let permit = acquire(&state).await?;
        run_with_permit(permit, work).await
    };
    match timeout {
        Some(limit) => tokio::time::timeout(limit, task).await.map_err(|_| {
            ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "the request ran past its time limit",
            )
        })?,
        None => task.await,
    }
}

/// Waits for one of the `max_concurrent` permits.
async fn acquire(state: &State) -> Result<OwnedSemaphorePermit, ApiError> {
    state.0.work.clone().acquire_owned().await.map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "shutting down",
        )
    })
}

/// Runs `work` on the blocking pool; the permit is released when the work ends.
async fn run_with_permit<T, F>(permit: OwnedSemaphorePermit, work: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        stacker::grow(WORK_STACK_BYTES, work)
    })
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "the engine failed on this request",
        )
    })?
}

async fn health(AxumState(state): AxumState<State>) -> Result<Json<Value>, ApiError> {
    let dir = state.dir().to_path_buf();
    let healthy = tokio::task::spawn_blocking(move || probe(&dir))
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false);
    if !healthy {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "the data directory is not readable and writable",
        ));
    }
    Ok(Json(json!({"status": "ok"})))
}

static PROBE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The data directory can be listed, and an empty file created in it and removed. The file is
/// empty so that a full volume still passes (a DELETE can then free space), and its name is unique
/// so that concurrent probes do not race.
pub fn probe(dir: &Path) -> std::io::Result<()> {
    std::fs::read_dir(dir)?;
    let n = PROBE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!(".healthz-probe-{}-{n}", std::process::id()));
    std::fs::File::create(&path)?;
    std::fs::remove_file(&path)
}

async fn list(AxumState(state): AxumState<State>) -> Result<Json<Value>, ApiError> {
    let dir = state.dir().to_path_buf();
    let datasets = run_blocking(&state, None, move || Ok(datasets::list(&dir)?)).await?;
    Ok(Json(json!({ "datasets": datasets })))
}

fn check_name(name: &str) -> Result<(), ApiError> {
    if datasets::valid_name(name) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_name",
            "a dataset name must match ^[a-z][a-z0-9_]{0,62}$",
        ))
    }
}

fn upload_format(query: &HashMap<String, String>, headers: &HeaderMap) -> Result<Format, ApiError> {
    let unsupported =
        |m: &str| ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_format", m);
    if let Some(value) = query.get("format") {
        return Format::from_query(value)
            .ok_or_else(|| unsupported("format must be csv, jsonl or parquet"));
    }
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(Format::from_content_type)
        .ok_or_else(|| {
            unsupported(
                "send Content-Type text/csv, application/x-ndjson or application/vnd.apache.parquet, or ?format=csv|jsonl|parquet",
            )
        })
}

#[derive(Serialize)]
struct UploadAnswer {
    #[serde(flatten)]
    dataset: datasets::DatasetInfo,
    replaced: bool,
}

async fn upload(
    AxumState(state): AxumState<State>,
    UrlPath(name): UrlPath<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    check_name(&name)?;
    let format = upload_format(&query, &headers)?;
    // The permit is taken before the body is read, so at most `max_concurrent` uploads are held in
    // memory at once; the read deadline stops a stalled upload from holding it for ever.
    let permit = acquire(&state).await?;
    let cap = state.0.config.upload_cap();
    let body = tokio::time::timeout(
        state.0.config.upload_timeout,
        read_body(&headers, body, cap),
    )
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::REQUEST_TIMEOUT,
            "timeout",
            "the upload did not arrive within its time limit",
        )
    })??;
    if body.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "the body is empty",
        ));
    }
    let dir = state.dir().to_path_buf();
    let decoded_cap = state.0.config.decoded_cap();
    // No timeout once the body is in: an upload that answered 504 would still land a moment later.
    let (dataset, replaced) = run_with_permit(permit, move || {
        Ok(datasets::store(&dir, &name, format, &body, decoded_cap)?)
    })
    .await?;
    let status = if replaced {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(UploadAnswer { dataset, replaced })).into_response())
}

async fn remove(
    AxumState(state): AxumState<State>,
    UrlPath(name): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    check_name(&name)?;
    let dir = state.dir().to_path_buf();
    let deleted = name.clone();
    tokio::task::spawn_blocking(move || datasets::delete(&dir, &name))
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "delete failed",
            )
        })??;
    Ok(Json(json!({ "deleted": deleted })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryRequest {
    sql: String,
}

#[derive(Serialize)]
struct QueryAnswer {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
}

fn sql_error(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "sql_error", e.to_string())
}

async fn query(
    AxumState(state): AxumState<State>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<QueryAnswer>, ApiError> {
    let timeout = state.0.config.query_timeout;
    let started = Instant::now();
    let body = tokio::time::timeout(timeout, read_body(&headers, body, QUERY_BODY_BYTES))
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::REQUEST_TIMEOUT,
                "timeout",
                "the query body did not arrive within the time limit",
            )
        })??;
    let request: QueryRequest = serde_json::from_slice(&body).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            format!("invalid JSON body: {e}"),
        )
    })?;
    if request.sql.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "sql is required",
        ));
    }
    let dir = state.dir().to_path_buf();
    let max_rows = state.0.config.max_rows;
    let remaining = timeout.saturating_sub(started.elapsed());
    // The sandbox runs on the work stack too: it walks the parsed query recursively.
    let answer = run_blocking(&state, Some(remaining), move || {
        match sandbox::check(&request.sql) {
            Ok(()) => {}
            Err(Refusal::Syntax(m)) => return Err(sql_error(m)),
            Err(Refusal::Forbidden(m)) => {
                return Err(ApiError::new(StatusCode::BAD_REQUEST, "forbidden_sql", m))
            }
        }
        let mut context = datasets::context(&dir)?;
        let plan = context.execute(&request.sql).map_err(sql_error)?;
        let frame = plan
            .limit(IdxSize::try_from(max_rows + 1).unwrap_or(IdxSize::MAX))
            .collect()
            .map_err(sql_error)?;
        let columns = frame
            .get_column_names()
            .into_iter()
            .map(|c| c.to_string())
            .collect();
        Ok(QueryAnswer {
            columns,
            rows: values::rows(&frame, max_rows),
            truncated: frame.height() > max_rows,
        })
    })
    .await?;
    Ok(Json(answer))
}

/// Serves `state` on `listener` until `shutdown` completes, then waits for open connections to
/// finish their requests. Unlike `axum::serve`, a client must send its headers within
/// `header_timeout`, and at most `max_connections` connections are open at once (one more is
/// closed as soon as it is accepted), so slow or idle clients cannot pile up.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: State,
    shutdown: impl std::future::Future<Output = ()>,
) -> std::io::Result<()> {
    use hyper_util::rt::{TokioIo, TokioTimer};
    use hyper_util::server::graceful::GracefulShutdown;
    use hyper_util::service::TowerToHyperService;

    let header_timeout = state.0.config.header_timeout;
    let connections = Arc::new(Semaphore::new(state.0.config.max_connections.max(1)));
    let app = router(state);
    let graceful = GracefulShutdown::new();
    tokio::pin!(shutdown);
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                // A transient accept failure (out of descriptors, a reset): pause, then go on.
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let mut builder = hyper::server::conn::http1::Builder::new();
        builder
            .timer(TokioTimer::new())
            .header_read_timeout(header_timeout);
        let connection =
            builder.serve_connection(TokioIo::new(stream), TowerToHyperService::new(app.clone()));
        let connection = graceful.watch(connection);
        tokio::spawn(async move {
            let _permit = permit;
            let _ = connection.await;
        });
    }
    drop(listener);
    graceful.shutdown().await;
    Ok(())
}
