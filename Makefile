IMG ?= oramasearch/lake-polars:dev

.PHONY: build test lint fmt docker-build run

build: ## Debug build for this machine.
	cargo build --locked

test: ## Unit and HTTP tests (no network, temp directories only).
	cargo test --locked

lint: ## Formatting and clippy, warnings are errors.
	cargo fmt --check
	cargo clippy --locked --all-targets -- -D warnings

fmt:
	cargo fmt

docker-build: ## linux/amd64 image, on native amd64. Never push a new name before it exists as a private repo (README).
	docker buildx build --platform linux/amd64 -t $(IMG) .

run: build ## Serve ./local-data on 127.0.0.1:8080 with a throwaway token.
	mkdir -p local-data
	LAKE_POLARS_TOKEN=local-dev-token LAKE_POLARS_DIR=./local-data LAKE_POLARS_ADDR=127.0.0.1:8080 ./target/debug/lake-polars
