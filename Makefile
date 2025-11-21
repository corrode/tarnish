.PHONY: help build test check clippy fmt doc examples clean publish

# Default target
.DEFAULT_GOAL := help

help: ## Show this help message
	@echo 'Usage: make [target]'
	@echo ''
	@echo 'Available targets:'
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z_-]+:.*?## / {printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build: ## Build the library
	cargo build --all-targets

release: ## Build in release mode
	cargo build --release --all-targets

test: ## Run all tests
	cargo test --all-targets

check: ## Quick check (faster than build)
	cargo check --all-targets

clippy: ## Run clippy lints
	cargo clippy --all-targets -- -D warnings

fmt: ## Format code
	cargo fmt --all

fmt-check: ## Check code formatting
	cargo fmt --all -- --check

doc: ## Build documentation
	cargo doc --no-deps --open

doc-private: ## Build documentation including private items
	cargo doc --no-deps --document-private-items --open

examples: ## Build all examples
	cargo build --examples

run-echo: ## Run the echo example
	cargo run --example echo

run-panic: ## Run the panic recovery example
	cargo run --example panic_recovery

run-calc: ## Run the safe computation example
	cargo run --example safe_computation

clean: ## Clean build artifacts
	cargo clean

verify: fmt-check clippy test ## Run all verification checks (format, clippy, tests)

publish-dry: ## Dry run of publish to crates.io
	cargo publish --dry-run

publish: ## Publish to crates.io
	cargo publish

watch: ## Watch for changes and rebuild
	cargo watch -x check -x test

bench: ## Run benchmarks (if any)
	cargo bench

bloat: ## Check binary size contributors
	cargo bloat --release

tree: ## Show dependency tree
	cargo tree

outdated: ## Check for outdated dependencies
	cargo outdated

audit: ## Security audit of dependencies
	cargo audit
