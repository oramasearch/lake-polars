// SPDX-License-Identifier: MIT

//! lake-polars, powered by Polars: Parquet datasets and Polars SQL over HTTP for lake's `polars`
//! service kind. The Orama operator runs it as a StatefulSet per service (orama-operator
//! `docs/lake/lake-polars.md`).
//!
//! Environment (docs/api.md has the whole table):
//!
//! ```text
//! LAKE_POLARS_TOKEN                 the bearer token (required)
//! LAKE_POLARS_DIR                   the dataset directory (default /data)
//! LAKE_POLARS_ADDR                  the listen address (default 0.0.0.0:8080)
//! LAKE_POLARS_MAX_BODY_BYTES        the largest upload (default 268435456, 256 MiB)
//! LAKE_POLARS_QUERY_TIMEOUT_SECONDS the query time limit (default 30)
//! LAKE_POLARS_UPLOAD_TIMEOUT_SECONDS how long an upload body may take to arrive (default 300)
//! LAKE_POLARS_MAX_CONCURRENT        queries and uploads running at once (default 2)
//! LAKE_POLARS_MEMORY_LIMIT          the container memory limit in bytes (optional)
//! LAKE_POLARS_THREADS               Polars' thread pool size (optional; POLARS_MAX_THREADS)
//! ```

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use lake_polars::{datasets, router, server, Config, State};
use serde_json::json;

fn log(level: &str, msg: &str, extra: serde_json::Value) {
    let mut line = json!({"level": level, "msg": msg});
    if let (Some(line), Some(extra)) = (line.as_object_mut(), extra.as_object()) {
        line.extend(extra.clone());
    }
    println!("{line}");
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn env_number<T: std::str::FromStr>(name: &str, default: T) -> Result<T, String> {
    match env(name) {
        None => Ok(default),
        Some(v) => v
            .parse()
            .map_err(|_| format!("{name} must be a positive number, got {v:?}")),
    }
}

fn config() -> Result<(Config, SocketAddr), String> {
    let token = env("LAKE_POLARS_TOKEN").ok_or("LAKE_POLARS_TOKEN is not set")?;
    let dir = PathBuf::from(env("LAKE_POLARS_DIR").unwrap_or_else(|| "/data".into()));
    let addr = env("LAKE_POLARS_ADDR").unwrap_or_else(|| "0.0.0.0:8080".into());
    let addr: SocketAddr = addr
        .parse()
        .map_err(|_| format!("LAKE_POLARS_ADDR must be host:port, got {addr:?}"))?;
    let mut config = Config::new(dir, token);
    config.max_body_bytes =
        env_number("LAKE_POLARS_MAX_BODY_BYTES", server::DEFAULT_MAX_BODY_BYTES)?;
    config.query_timeout = Duration::from_secs(env_number(
        "LAKE_POLARS_QUERY_TIMEOUT_SECONDS",
        server::DEFAULT_QUERY_TIMEOUT.as_secs(),
    )?);
    config.upload_timeout = Duration::from_secs(env_number(
        "LAKE_POLARS_UPLOAD_TIMEOUT_SECONDS",
        server::DEFAULT_UPLOAD_TIMEOUT.as_secs(),
    )?);
    config.max_concurrent =
        env_number("LAKE_POLARS_MAX_CONCURRENT", server::DEFAULT_MAX_CONCURRENT)?;
    config.memory_limit = match env("LAKE_POLARS_MEMORY_LIMIT") {
        None => None,
        Some(_) => Some(env_number::<u64>("LAKE_POLARS_MEMORY_LIMIT", 0)?).filter(|n| *n > 0),
    };
    if config.max_body_bytes == 0
        || config.query_timeout.is_zero()
        || config.upload_timeout.is_zero()
        || config.max_concurrent == 0
    {
        return Err("limits must be greater than zero".into());
    }
    config.log = Arc::new(|line| println!("{line}"));
    Ok((config, addr))
}

fn main() -> ExitCode {
    // Polars sizes its thread pool from POLARS_MAX_THREADS the first time it is used, so this must
    // happen before any Polars call and before other threads exist.
    if let Some(threads) = env("LAKE_POLARS_THREADS") {
        if threads.parse::<usize>().map(|n| n > 0) != Ok(true) {
            log(
                "ERROR",
                "lake-polars stopped",
                json!({"err": "LAKE_POLARS_THREADS must be a positive number"}),
            );
            return ExitCode::FAILURE;
        }
        std::env::set_var("POLARS_MAX_THREADS", threads);
    }
    let (config, addr) = match config() {
        Ok(c) => c,
        Err(err) => {
            log("ERROR", "lake-polars stopped", json!({"err": err}));
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            log(
                "ERROR",
                "lake-polars stopped",
                json!({"err": e.to_string()}),
            );
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(serve(config, addr));
    // Work still running after the listener closed (a query past its timeout, an upload) gets a
    // bounded time to finish; an interrupted upload leaves only a temp file, swept at next start.
    runtime.shutdown_timeout(server::DEFAULT_QUERY_TIMEOUT);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            log("ERROR", "lake-polars stopped", json!({"err": err}));
            ExitCode::FAILURE
        }
    }
}

async fn serve(config: Config, addr: SocketAddr) -> Result<(), String> {
    let dir = config.dir.clone();
    server::probe(&dir)
        .map_err(|e| format!("the data directory {} is not usable: {e}", dir.display()))?;
    let swept = datasets::sweep_temp_files(&dir).map_err(|e| e.to_string())?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("listen on {addr}: {e}"))?;
    log(
        "INFO",
        "lake-polars listening",
        json!({"addr": addr.to_string(), "dir": dir.display().to_string(), "swept_temp_files": swept,
               "upload_cap": config.upload_cap(), "max_concurrent": config.max_concurrent}),
    );
    axum::serve(listener, router(State::new(config)))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| e.to_string())?;
    log("INFO", "lake-polars stopped serving", json!({}));
    Ok(())
}

async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {},
        () = terminate => {},
    }
    log("INFO", "lake-polars shutting down", json!({}));
}
