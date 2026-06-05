.DEFAULT_GOAL := help

DASHBOARD := bench/dashboard
DOCS := docs
SCENARIO ?= connection-storm
SECONDS ?= 5
EX ?= headless_run

.PHONY: help
help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

.PHONY: dashboard
dashboard: ## Start a node + the dashboard, then open the printed URL — "the money"
	@cargo build -p rusm-cli
	@echo "→ starting node (log: /tmp/rusm-node.log) + dashboard…"
	@./target/debug/rusm node start >/tmp/rusm-node.log 2>&1 & \
		NODE=$$!; \
		trap 'kill $$NODE 2>/dev/null' EXIT INT TERM; \
		cd $(DASHBOARD) && { test -d node_modules || bun install; } && bun run dev

.PHONY: node
node: ## Start a RUSM node on ws://127.0.0.1:4000
	cargo run -p rusm-cli -- node start

.PHONY: ui
ui: ## Start only the dashboard dev server (expects a node already running)
	cd $(DASHBOARD) && { test -d node_modules || bun install; } && bun run dev

.PHONY: attach
attach: ## Attach a live REPL to the local node
	cargo run -p rusm-cli -- attach

.PHONY: run
run: ## Run a scenario in the terminal (SCENARIO=… SECONDS=…)
	cargo run -p rusm-bench -- run $(SCENARIO) $(SECONDS)

.PHONY: example
example: ## Run an example (EX=headless_run|synthetic_source|observer_overhead|embedded_node)
	cargo run -p rusm-bench --example $(EX)

.PHONY: build
build: ## Build the whole workspace
	cargo build --workspace

.PHONY: test
test: ## Run all Rust + dashboard tests
	cargo test --workspace
	cd $(DASHBOARD) && bun test

.PHONY: cov
cov: ## Coverage report (Rust workspace + dashboard)
	cargo llvm-cov --workspace --ignore-filename-regex 'main\.rs' --summary-only
	cd $(DASHBOARD) && bun test --coverage

.PHONY: fmt
fmt: ## Format Rust + dashboard
	cargo fmt
	cd $(DASHBOARD) && bunx prettier --write src

.PHONY: fmt-check
fmt-check: ## Check formatting (Rust + dashboard)
	cargo fmt --check
	cd $(DASHBOARD) && bunx prettier --check src

.PHONY: docs
docs: ## Live-preview the documentation site
	cd $(DOCS) && { test -d node_modules || bun install; } && bun run dev

.PHONY: docs-build
docs-build: ## Build the static documentation site
	cd $(DOCS) && { test -d node_modules || bun install; } && bun run build

.PHONY: clean
clean: ## Remove Rust build artifacts
	cargo clean
