// SPDX-License-Identifier: MIT

//! The HTTP API end to end, over the router in memory and a temporary data directory.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use lake_polars::{router, Config, State};
use polars::prelude::*;
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "test-token-0123456789abcdef";

struct Harness {
    app: Router,
    dir: TempDir,
    logs: Arc<Mutex<Vec<String>>>,
}

fn harness_with(adjust: impl FnOnce(&mut Config)) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let logs = Arc::new(Mutex::new(Vec::new()));
    let mut config = Config::new(dir.path(), TOKEN);
    let sink = logs.clone();
    config.log = Arc::new(move |line| sink.lock().unwrap().push(line.to_string()));
    adjust(&mut config);
    Harness {
        app: router(State::new(config)),
        dir,
        logs,
    }
}

fn harness() -> Harness {
    harness_with(|_| {})
}

impl Harness {
    async fn send(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        content_type: Option<&str>,
        body: Vec<u8>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(content_type) = content_type {
            request = request.header(header::CONTENT_TYPE, content_type);
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(&bytes)))
        };
        (status, value)
    }

    async fn upload(&self, name: &str, content_type: &str, body: &[u8]) -> (StatusCode, Value) {
        self.send(
            "POST",
            &format!("/v1/datasets/{name}"),
            Some(TOKEN),
            Some(content_type),
            body.to_vec(),
        )
        .await
    }

    async fn query(&self, sql: &str) -> (StatusCode, Value) {
        self.send(
            "POST",
            "/v1/query",
            Some(TOKEN),
            Some("application/json"),
            serde_json::to_vec(&json!({ "sql": sql })).unwrap(),
        )
        .await
    }
}

const CUSTOMERS_CSV: &str = "id,name,city\n1,ada,london\n2,grace,new york\n3,linus,helsinki\n";
const ORDERS_JSONL: &str = "{\"order_id\":10,\"customer_id\":1,\"amount\":2.5}\n\
{\"order_id\":11,\"customer_id\":1,\"amount\":4.0}\n\
{\"order_id\":12,\"customer_id\":2,\"amount\":10.25}\n";

fn parquet_bytes() -> Vec<u8> {
    let mut frame = df!(
        "sku" => ["a", "b", "c", "d"],
        "price" => [1.5f64, 2.0, 3.25, 4.0],
        "stock" => [10i64, 0, 5, 7],
    )
    .unwrap();
    let mut out = Vec::new();
    ParquetWriter::new(&mut out).finish(&mut frame).unwrap();
    out
}

fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("")
}

#[tokio::test]
async fn uploads_every_format_and_lists_them() {
    let h = harness();
    let (status, body) = h
        .upload(
            "customers",
            "text/csv; charset=utf-8",
            CUSTOMERS_CSV.as_bytes(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["name"], "customers");
    assert_eq!(body["rows"], 3);
    assert_eq!(body["replaced"], false);
    assert_eq!(
        body["columns"],
        json!([{"name":"id","type":"i64"},{"name":"name","type":"str"},{"name":"city","type":"str"}])
    );
    assert!(body["bytes"].as_u64().unwrap() > 0);

    let (status, body) = h
        .upload("orders", "application/x-ndjson", ORDERS_JSONL.as_bytes())
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["rows"], 3);

    let (status, body) = h
        .upload(
            "products",
            "application/vnd.apache.parquet",
            &parquet_bytes(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["rows"], 4);

    // ?format= wins over the Content-Type.
    let (status, body) = h
        .send(
            "POST",
            "/v1/datasets/products_copy?format=parquet",
            Some(TOKEN),
            Some("application/octet-stream"),
            parquet_bytes(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // Every dataset is stored as Parquet under its name, and nothing else is left behind.
    let mut files: Vec<String> = std::fs::read_dir(h.dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "customers.parquet",
            "orders.parquet",
            "products.parquet",
            "products_copy.parquet"
        ]
    );

    let (status, body) = h
        .send("GET", "/v1/datasets", Some(TOKEN), None, vec![])
        .await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = body["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["customers", "orders", "products", "products_copy"]);
    let orders = &body["datasets"][1];
    assert_eq!(orders["rows"], 3);
    assert_eq!(
        orders["columns"],
        json!([{"name":"order_id","type":"i64"},{"name":"customer_id","type":"i64"},{"name":"amount","type":"f64"}])
    );
}

#[tokio::test]
async fn an_upload_replaces_a_dataset_of_the_same_name() {
    let h = harness();
    assert_eq!(
        h.upload("t", "text/csv", b"x\n1\n").await.0,
        StatusCode::CREATED
    );
    let (status, body) = h.upload("t", "text/csv", b"x\n1\n2\n").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["replaced"], true);
    assert_eq!(body["rows"], 2);
}

#[tokio::test]
async fn queries_join_and_aggregate() {
    let h = harness();
    h.upload("customers", "text/csv", CUSTOMERS_CSV.as_bytes())
        .await;
    h.upload("orders", "application/jsonl", ORDERS_JSONL.as_bytes())
        .await;

    let (status, body) = h
        .query(
            "SELECT c.name, count(*) AS orders, sum(o.amount) AS total \
             FROM customers c JOIN orders o ON c.id = o.customer_id \
             GROUP BY c.name ORDER BY total DESC",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["columns"], json!(["name", "orders", "total"]));
    assert_eq!(body["rows"], json!([["grace", 1, 10.25], ["ada", 2, 6.5]]));
    assert!(body.get("truncated").is_none());

    let (status, body) = h
        .query("SELECT avg(amount) AS mean, max(order_id) AS last FROM orders")
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rows"], json!([[5.583333333333333, 12]]));

    let (status, body) = h.query("SELECT name FROM customers WHERE id > 99").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"columns": ["name"], "rows": []}));
}

#[tokio::test]
async fn value_types_come_back_as_json_types() {
    let h = harness();
    h.upload(
        "v",
        "application/x-ndjson",
        b"{\"i\":1,\"f\":1.0,\"s\":\"x\",\"b\":true,\"n\":null}\n",
    )
    .await;
    let (status, body) = h
        .query(
            "SELECT i, f, s, b, n, CAST('2026-09-26' AS DATE) AS d, \
             CAST('2026-09-26T10:11:12' AS TIMESTAMP) AS ts FROM v",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["rows"],
        json!([[1, 1.0, "x", true, null, "2026-09-26", "2026-09-26T10:11:12"]])
    );
}

#[tokio::test]
async fn results_are_capped_and_marked_truncated() {
    let h = harness_with(|c| c.max_rows = 5);
    let mut csv = String::from("n\n");
    for i in 0..20 {
        csv.push_str(&format!("{i}\n"));
    }
    h.upload("numbers", "text/csv", csv.as_bytes()).await;
    let (status, body) = h.query("SELECT n FROM numbers ORDER BY n").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["rows"].as_array().unwrap().len(), 5);
    assert_eq!(body["rows"][4], json!([4]));
    assert_eq!(body["truncated"], true);

    // Exactly at the cap is not truncated.
    let (_, body) = h.query("SELECT n FROM numbers WHERE n < 5").await;
    assert_eq!(body["rows"].as_array().unwrap().len(), 5);
    assert!(body.get("truncated").is_none());
}

#[tokio::test]
async fn the_token_is_required_everywhere_but_healthz() {
    let h = harness();
    for (method, path) in [
        ("GET", "/v1/datasets"),
        ("POST", "/v1/datasets/x"),
        ("DELETE", "/v1/datasets/x"),
        ("POST", "/v1/query"),
        ("GET", "/nowhere"),
    ] {
        for token in [
            None,
            Some("wrong"),
            Some(&TOKEN[..10]),
            Some(&*format!("{TOKEN}x")),
        ] {
            let (status, body) = h.send(method, path, token, None, vec![]).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {path} {token:?}"
            );
            assert_eq!(error_code(&body), "unauthorized");
        }
    }
    // Another scheme with the right token is refused too.
    let request = Request::get("/v1/datasets")
        .header(header::AUTHORIZATION, format!("Basic {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let response = h.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().contains_key(header::WWW_AUTHENTICATE));

    let (status, body) = h.send("GET", "/healthz", None, None, vec![]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"status": "ok"}));
}

#[tokio::test]
async fn healthz_fails_when_the_directory_is_gone() {
    let h = harness();
    std::fs::remove_dir_all(h.dir.path()).unwrap();
    let (status, body) = h.send("GET", "/healthz", None, None, vec![]).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error_code(&body), "unavailable");
}

#[tokio::test]
async fn delete_removes_a_dataset() {
    let h = harness();
    h.upload("gone", "text/csv", b"a\n1\n").await;
    let (status, body) = h
        .send("DELETE", "/v1/datasets/gone", Some(TOKEN), None, vec![])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"deleted": "gone"}));
    assert!(!h.dir.path().join("gone.parquet").exists());

    let (status, body) = h
        .send("DELETE", "/v1/datasets/gone", Some(TOKEN), None, vec![])
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "not_found");

    let (status, body) = h.query("SELECT * FROM gone").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "sql_error");
}

#[tokio::test]
async fn names_formats_and_bodies_are_validated() {
    let h = harness();
    for name in [
        "Upper",
        "1abc",
        "a-b",
        "a.b",
        "..",
        "_x",
        "%2e%2e",
        &"a".repeat(64),
    ] {
        let (status, body) = h.upload(name, "text/csv", b"a\n1\n").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
        assert_eq!(error_code(&body), "invalid_name", "{name}");
    }
    // A path that tries to leave the directory never reaches the handler as a name.
    let (status, _) = h.upload("../etc", "text/csv", b"a\n1\n").await;
    assert_ne!(status, StatusCode::CREATED);
    assert_eq!(std::fs::read_dir(h.dir.path()).unwrap().count(), 0);

    let (status, body) = h.upload("x", "application/octet-stream", b"a\n1\n").await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(error_code(&body), "unsupported_format");
    let (status, body) = h
        .send(
            "POST",
            "/v1/datasets/x?format=xlsx",
            Some(TOKEN),
            Some("text/csv"),
            b"a\n1\n".to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(error_code(&body), "unsupported_format");

    let (status, body) = h
        .upload("x", "application/vnd.apache.parquet", b"not parquet")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_data");
    let (status, body) = h.upload("x", "text/csv", b"").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_request");
    // A failed upload leaves no file behind.
    assert_eq!(std::fs::read_dir(h.dir.path()).unwrap().count(), 0);

    let (status, body) = h
        .send(
            "POST",
            "/v1/query",
            Some(TOKEN),
            None,
            b"{\"sql\":1}".to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_request");
    let (status, body) = h
        .send(
            "POST",
            "/v1/query",
            Some(TOKEN),
            None,
            b"{\"sql\":\"SELECT 1\",\"x\":1}".to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_request");
    let (status, body) = h.query("  ").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_request");
    let (status, body) = h.query("SELEC nothing").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "sql_error");

    let (status, body) = h.send("GET", "/v1/query", Some(TOKEN), None, vec![]).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(error_code(&body), "method_not_allowed");
    let (status, body) = h
        .send("GET", "/v1/nowhere", Some(TOKEN), None, vec![])
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "not_found");
}

#[tokio::test]
async fn bodies_over_the_cap_are_refused() {
    let h = harness_with(|c| c.max_body_bytes = 64);
    let big = format!("a\n{}", "1\n".repeat(100));
    let (status, body) = h.upload("big", "text/csv", big.as_bytes()).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error_code(&body), "body_too_large");

    // The memory guard lowers the cap to a quarter of the memory limit.
    let h = harness_with(|c| c.memory_limit = Some(256));
    let (status, _) = h.upload("big", "text/csv", big.as_bytes()).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let (status, _) = h.upload("small", "text/csv", b"a\n1\n").await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn file_reading_table_functions_are_refused() {
    let h = harness();
    h.upload("t", "text/csv", b"a\n1\n").await;
    let secret = h.dir.path().join("secret.csv");
    std::fs::write(&secret, "password\nhunter2\n").unwrap();
    let secret = secret.display().to_string();
    for sql in [
        "SELECT * FROM read_csv('/etc/passwd')".to_string(),
        format!("SELECT * FROM read_csv('{secret}')"),
        format!(
            "SELECT * FROM read_parquet('{}')",
            h.dir.path().join("t.parquet").display()
        ),
        "SELECT * FROM read_json('/etc/hosts')".to_string(),
        "SELECT * FROM read_ipc('/etc/hosts')".to_string(),
        "SELECT * FROM t WHERE a IN (SELECT 1 FROM read_csv('/etc/passwd'))".to_string(),
        "WITH x AS (SELECT * FROM read_csv('/etc/passwd')) SELECT * FROM x".to_string(),
        "CREATE TABLE x AS SELECT * FROM read_csv('/etc/passwd')".to_string(),
        "SELECT 1; SELECT 2".to_string(),
    ] {
        let (status, body) = h.query(&sql).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{sql}");
        assert_eq!(error_code(&body), "forbidden_sql", "{sql}: {body}");
        assert!(!body.to_string().contains("hunter2"));
        assert!(!body.to_string().contains("root:"));
    }
    // A file in the directory that is not a dataset is not a table either.
    let (status, body) = h.query("SELECT * FROM secret").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "sql_error");
}

#[tokio::test]
async fn a_query_past_its_time_limit_answers_504() {
    let h = harness_with(|c| {
        c.query_timeout = Duration::from_millis(1);
        c.max_concurrent = 1;
    });
    let mut csv = String::from("a,b\n");
    for i in 0..20_000 {
        csv.push_str(&format!("{i},{}\n", i % 97));
    }
    h.upload("t", "text/csv", csv.as_bytes()).await;
    let (status, body) = h
        .query("SELECT x.a AS xa, y.a AS ya FROM t x JOIN t y ON x.b = y.b ORDER BY x.a, y.a")
        .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");
    assert_eq!(error_code(&body), "timeout");
}

#[tokio::test]
async fn the_log_never_carries_sql_data_or_the_token() {
    let h = harness();
    h.upload("secrets", "text/csv", b"code\nsupersecretvalue\n")
        .await;
    h.query("SELECT code FROM secrets WHERE code = 'marker_in_sql'")
        .await;
    h.send(
        "GET",
        "/v1/datasets",
        Some("wrong-token-value"),
        None,
        vec![],
    )
    .await;
    let logs = h.logs.lock().unwrap();
    assert_eq!(logs.len(), 3);
    for line in logs.iter() {
        let entry: Value = serde_json::from_str(line).unwrap();
        for key in ["method", "path", "status", "duration_ms"] {
            assert!(entry.get(key).is_some(), "{line}");
        }
        for secret in [
            TOKEN,
            "wrong-token-value",
            "supersecretvalue",
            "marker_in_sql",
            "SELECT",
        ] {
            assert!(!line.contains(secret), "{line}");
        }
    }
}

/// An upload holds a permit while its body arrives, so concurrent uploads cannot buffer more than
/// `max_concurrent` bodies; one that stalls is cut off at the upload time limit with `408`.
#[tokio::test(flavor = "multi_thread")]
async fn an_upload_holds_its_permit_while_it_arrives_and_times_out() {
    use std::io::{Read, Write};

    let h = harness_with(|c| {
        c.max_concurrent = 1;
        c.query_timeout = Duration::from_millis(300);
        c.upload_timeout = Duration::from_millis(1500);
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = h.app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // The headers promise 100 bytes; only 4 ever come.
    let mut stalled = std::net::TcpStream::connect(addr).unwrap();
    write!(
        stalled,
        "POST /v1/datasets/slow HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {TOKEN}\r\n\
         Content-Type: text/csv\r\nContent-Length: 100\r\n\r\na,b\n"
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let (status, body) = h.query("SELECT 1 AS one").await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");

    let answer = tokio::task::spawn_blocking(move || {
        stalled
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut buf = [0u8; 512];
        let n = stalled.read(&mut buf).unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    })
    .await
    .unwrap();
    assert!(answer.starts_with("HTTP/1.1 408"), "{answer}");
    assert!(answer.contains("\"timeout\""), "{answer}");

    let (status, body) = h.query("SELECT 1 AS one").await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
