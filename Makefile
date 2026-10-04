# Rift developer commands. Cargo lives in ~/.cargo/bin (rustup) unless on PATH.
CARGO ?= $(shell command -v cargo 2>/dev/null || echo $(HOME)/.cargo/bin/cargo)

.PHONY: help doctor doctor-live canon-doctor canon-doctor-live director-preflight backend test test-backend test-canon smoke format lint lint-tiger tiger-live check

help:
	@echo "make doctor        system + provider status (doctor-live adds connectivity checks)"
	@echo "make canon-doctor  provider configuration (add -live for connectivity)"
	@echo "make director-preflight  one tiny live request per configured Director model"
	@echo "make backend       run the Rust backend on BACKEND_HOST:BACKEND_PORT"
	@echo "make test          Rust + Python tests"
	@echo "make smoke         start backend, run WebSocket smoke client, stop"
	@echo "make format        cargo fmt + ruff format"
	@echo "make lint          fmt --check, clippy -D warnings, ruff check"
	@echo "make check         lint + test + smoke"
	@echo "make lint-tiger    clippy + tests with the Tiger Data feature (offline)"
	@echo "make tiger-live    live Tiger Data round trip (needs TIGER_DATABASE_URL)"

doctor:
	@./scripts/doctor.sh

doctor-live:
	@./scripts/doctor.sh --live

canon-doctor:
	@cd canon && uv run python -m rift_canon.doctor

canon-doctor-live:
	@cd canon && uv run python -m rift_canon.doctor --live

director-preflight:
	@$(CARGO) run -q --manifest-path backend/Cargo.toml --example director_preflight

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

lint-tiger:
	$(CARGO) clippy --manifest-path backend/Cargo.toml --all-targets --features tiger -- -D warnings
	$(CARGO) test --manifest-path backend/Cargo.toml --features tiger

tiger-live:
	$(CARGO) test --manifest-path backend/Cargo.toml --features tiger --test tiger_live -- --ignored --nocapture

check: lint test smoke
