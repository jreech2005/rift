# Rift developer commands. Cargo lives in ~/.cargo/bin (rustup) unless on PATH.
CARGO ?= $(shell command -v cargo 2>/dev/null || echo $(HOME)/.cargo/bin/cargo)

.PHONY: demo demo-new demo-breaking-bad demo-matrix demo-harry-potter demo-stop demo-status demo-logs demo-preflight
.PHONY: help doctor doctor-live canon-doctor canon-doctor-live director-preflight backend test test-backend test-canon voice-live smoke format lint lint-tiger tiger-live check

help:
	@echo "make demo TITLE=\"The Matrix\"  one-command demo: backend + Unreal (docs/DEMO.md)"
	@echo "make demo-stop | demo-status | demo-logs | demo-preflight"
	@echo "make doctor        system + provider status (doctor-live adds connectivity checks)"
	@echo "make canon-doctor  provider configuration (add -live for connectivity)"
	@echo "make director-preflight  one tiny live request per configured Director model"
	@echo "make backend       run the Rust backend on BACKEND_HOST:BACKEND_PORT"
	@echo "make test          Rust + Python tests"
	@echo "make voice-live    live ElevenLabs synthesis check (needs credentials)"
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

# Live ElevenLabs synthesis. Needs ELEVENLABS_API_KEY and ELEVENLABS_VOICES;
# fails with BLOCKED without them. Spends a few characters of quota.
voice-live:
	$(CARGO) test --manifest-path backend/Cargo.toml --test voice_live -- --ignored --nocapture

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

# --- Demo launcher (docs/DEMO.md) ---------------------------------------------
# TITLE reaches the launcher through the environment, so quotes and
# apostrophes in a title need no escaping. NO_UNREAL=1: backend only.
DEMO := python3 scripts/rift_demo.py
DEMO_FLAGS := $(if $(NO_UNREAL),--no-unreal)
export TITLE

demo:
	@$(DEMO) start --title "$$TITLE" $(DEMO_FLAGS)

# May call TMDB, Wikipedia and Gemini. Not the judge path.
demo-new:
	@$(DEMO) start --new --title "$$TITLE" $(DEMO_FLAGS)

demo-breaking-bad:
	@$(DEMO) start --title "Breaking Bad" $(DEMO_FLAGS)

demo-matrix:
	@$(DEMO) start --title "The Matrix" $(DEMO_FLAGS)

demo-harry-potter:
	@$(DEMO) start --title "Harry Potter and the Philosopher's Stone" $(DEMO_FLAGS)

demo-stop:
	@$(DEMO) stop

demo-status:
	@$(DEMO) status

demo-logs:
	@$(DEMO) logs

# Offline: local prerequisites and PRESENT/MISSING configuration. No API calls.
demo-preflight:
	@$(DEMO) preflight
