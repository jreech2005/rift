# Rift developer commands. Cargo lives in ~/.cargo/bin (rustup) unless on PATH.
CARGO ?= $(shell command -v cargo 2>/dev/null || echo $(HOME)/.cargo/bin/cargo)

.PHONY: help doctor canon-doctor canon-doctor-live backend test test-backend test-canon smoke format lint check

help:
	@echo "make doctor        system + provider status"
	@echo "make canon-doctor  provider configuration (add -live for connectivity)"
	@echo "make backend       run the Rust backend on BACKEND_HOST:BACKEND_PORT"
	@echo "make test          Rust + Python tests"
	@echo "make smoke         start backend, run WebSocket smoke client, stop"
	@echo "make format        cargo fmt + ruff format"
	@echo "make lint          fmt --check, clippy -D warnings, ruff check"
	@echo "make check         lint + test + smoke"

doctor:
	@./scripts/doctor.sh

canon-doctor:
	@cd canon && uv run python -m rift_canon.doctor

canon-doctor-live:
	@cd canon && uv run python -m rift_canon.doctor --live

backend:
	$(CARGO) run --manifest-path backend/Cargo.toml

test: test-backend test-canon

test-backend:
	$(CARGO) test --manifest-path backend/Cargo.toml

test-canon:
	cd canon && uv run pytest -q

smoke:
	@./scripts/smoke.sh

format:
	$(CARGO) fmt --manifest-path backend/Cargo.toml
	cd canon && uv run ruff format . && uv run ruff check --fix .

lint:
	$(CARGO) fmt --manifest-path backend/Cargo.toml --check
	$(CARGO) clippy --manifest-path backend/Cargo.toml --all-targets -- -D warnings
	cd canon && uv run ruff format --check . && uv run ruff check .

check: lint test smoke
