// Copyright 2026 OramaSearch.
// Licensed under the Apache License, Version 2.0.

//! lake-polars, powered by Polars: the web service behind lake's `polars` service kind.
//!
//! Datasets (CSV, JSONL or Parquet uploads, stored as Parquet) live in one directory; `POST
//! /v1/query` runs Polars SQL over them. The contract is lake `docs/control-plane/services.md`
//! (the `polars` row and the paragraph after the table); `docs/api.md` is the API as built.

pub mod datasets;
pub mod sandbox;
pub mod server;
pub mod values;

pub use server::{router, Config, State};
