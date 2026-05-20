CARGO := cargo

BOLD  := \033[1m
CYAN  := \033[36m
GREEN := \033[32m
RESET := \033[0m

.DEFAULT_GOAL := help

# ── Build ────────────────────────────────────────────────────────────────────

.PHONY: build release release-mini release-full install-mini install-full

build: ## Debug build
	$(CARGO) build

release: ## Release build (LTO + strip, default features)
	$(CARGO) build --release

release-mini: ## Release build — FTS-only, no semantic search
	$(CARGO) build --release --no-default-features
	@printf '\n$(GREEN)  ✓ recall-mini built$(RESET)  ./target/release/recall\n\n'

release-full: ## Release build — hybrid FTS + semantic (fastembed, default)
	$(CARGO) build --release
	@printf '\n$(GREEN)  ✓ recall-full (fastembed) built$(RESET)  ./target/release/recall\n\n'

release-candle: ## Release build — hybrid FTS + semantic (candle backend, Metal/CUDA-capable)
	$(CARGO) build --release --no-default-features --features semantic-search,semantic-candle
	@printf '\n$(GREEN)  ✓ recall-full (candle) built$(RESET)  ./target/release/recall\n\n'

# Each install target ships a DISTINCTLY-NAMED binary so the user can keep
# all three available side-by-side and pick per-invocation:
#   recall-mini       # FTS only
#   recall-fastembed  # hybrid via ONNX (no openssl)
#   recall-candle     # hybrid via candle (Metal/CUDA)
# Symlink whichever you use most as `recall` if you want a default.

install-mini: release-mini ## Build + install to ~/.cargo/bin/recall-mini
	@install -m 755 ./target/release/recall $$HOME/.cargo/bin/recall-mini
	@printf '$(GREEN)  installed $$HOME/.cargo/bin/recall-mini$(RESET)\n'
	@$$HOME/.cargo/bin/recall-mini info | grep -E "Variant|Version" || true

install-fastembed: release-full ## Build + install to ~/.cargo/bin/recall-fastembed
	@install -m 755 ./target/release/recall $$HOME/.cargo/bin/recall-fastembed
	@printf '$(GREEN)  installed $$HOME/.cargo/bin/recall-fastembed$(RESET)\n'
	@$$HOME/.cargo/bin/recall-fastembed info | grep -E "Variant|Version" || true

install-candle: release-candle ## Build + install to ~/.cargo/bin/recall-candle
	@install -m 755 ./target/release/recall $$HOME/.cargo/bin/recall-candle
	@printf '$(GREEN)  installed $$HOME/.cargo/bin/recall-candle$(RESET)\n'
	@$$HOME/.cargo/bin/recall-candle info | grep -E "Variant|Version" || true

install-all: install-mini install-fastembed install-candle ## Build + install all 3 variants
	@printf '\n$(GREEN)  All three variants installed.$(RESET)\n'
	@printf '  Symlink your default: ln -sf $$HOME/.cargo/bin/recall-fastembed $$HOME/.cargo/bin/recall\n'

# Legacy alias — kept so older docs/scripts still work; installs the full
# (fastembed) build to ~/.cargo/bin/recall.
install-full: release-full ## (deprecated) Build full + install as plain `recall`
	@install -m 755 ./target/release/recall $$HOME/.cargo/bin/recall
	@printf '$(GREEN)  installed $$HOME/.cargo/bin/recall (fastembed full)$(RESET)\n'
	@$$HOME/.cargo/bin/recall info | grep -E "Variant|Version" || true

# ── Quality ──────────────────────────────────────────────────────────────────

.PHONY: check test lint fmt

check: ## Full quality gate — format, lint, test
	@printf '\n$(BOLD)[1/3] Checking format$(RESET)\n'
	$(CARGO) fmt -- --check
	@printf '\n$(BOLD)[2/3] Running clippy$(RESET)\n'
	$(CARGO) clippy --all-targets -- -D warnings
	@printf '\n$(BOLD)[3/3] Running tests$(RESET)\n'
	$(CARGO) test
	@printf '\n$(GREEN)  ✓ All checks passed$(RESET)\n\n'

test: ## Run tests
	$(CARGO) test

lint: ## Run clippy
	$(CARGO) clippy --all-targets -- -D warnings

fmt: ## Format code
	$(CARGO) fmt

# ── Documentation ────────────────────────────────────────────────────────────

.PHONY: doc

doc: ## Generate API documentation
	$(CARGO) doc --no-deps

# ── Install ──────────────────────────────────────────────────────────────────

.PHONY: install uninstall

install: ## Install binary to ~/.cargo/bin
	$(CARGO) install --path .

uninstall: ## Remove installed binary
	$(CARGO) uninstall recall

# ── Run ──────────────────────────────────────────────────────────────────────

.PHONY: run sync search

run: ## Launch TUI
	$(CARGO) run

sync: ## Incremental sync (use FORCE=1 to reprocess all)
	$(CARGO) run -- sync $(if $(FORCE),--force,)

search: ## Search sessions (Q="query")
	@test -n "$(Q)" || { printf 'Usage: make search Q="query"\n'; exit 1; }
	$(CARGO) run -- search "$(Q)"

# ── Release ──────────────────────────────────────────────────────────────────

.PHONY: release-patch release-minor release-major

release-patch: ## Bump patch + commit + tag + push (append EXECUTE=1 to apply)
	cargo release patch $(if $(EXECUTE),--execute,)

release-minor: ## Bump minor + commit + tag + push (append EXECUTE=1 to apply)
	cargo release minor $(if $(EXECUTE),--execute,)

release-major: ## Bump major + commit + tag + push (append EXECUTE=1 to apply)
	cargo release major $(if $(EXECUTE),--execute,)

# ── Maintenance ──────────────────────────────────────────────────────────────

.PHONY: clean

clean: ## Remove build artifacts
	$(CARGO) clean

# ── Help ─────────────────────────────────────────────────────────────────────

.PHONY: help

help: ## Show available targets
	@awk 'BEGIN {FS = ":.*## "; printf "\n$(BOLD)Recall$(RESET) — local-first AI session search\n"} \
		/^# ── / {n = $$0; gsub(/(^# ── | ─+$$)/, "", n); printf "\n$(BOLD)%s$(RESET)\n", n} \
		/^[a-zA-Z_-]+:.*## / {printf "  $(CYAN)make %-12s$(RESET) %s\n", $$1, $$2} \
		END {printf "\n"}' $(MAKEFILE_LIST)
