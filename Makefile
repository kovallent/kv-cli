# Requires a C compiler (cc) to build the tree-sitter Python grammar:
# Xcode Command Line Tools on macOS, build-essential or equivalent on Linux.

BIN         := kv-cli
CARGO       ?= cargo
TARGET_DIR  ?= target
RELEASE_BIN := $(TARGET_DIR)/release/$(BIN)
PREFIX      ?= /usr/local

.DEFAULT_GOAL := release

.PHONY: help
help: ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
	  | awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

.PHONY: build
build: ## Debug build
	$(CARGO) build

.PHONY: release
release: ## Optimized release binary (LTO, stripped)
	$(CARGO) build --release
	@echo
	@echo "binary: $(RELEASE_BIN)"
	@ls -lh $(RELEASE_BIN) | awk '{print "size:   " $$5}'

.PHONY: test
test: ## Run the unit test suite
	$(CARGO) test

.PHONY: check
check: ## Format check, clippy, and tests
	$(CARGO) fmt --check
	$(CARGO) clippy --all-targets -- -D warnings
	$(CARGO) test

.PHONY: fmt
fmt: ## Format the source tree
	$(CARGO) fmt

.PHONY: demo
demo: release ## Audit the bundled sample project (expected to fail)
	@echo "--- kv-cli audit samples/ ---"
	@./$(RELEASE_BIN) audit samples/ || echo "exit code: $$? (expected 1)"

.PHONY: install
install: release ## Install to $(PREFIX)/bin
	install -m 0755 $(RELEASE_BIN) $(PREFIX)/bin/$(BIN)

.PHONY: clean
clean: ## Remove build artifacts and fix backups
	$(CARGO) clean
	@find . -name '*.kvbak' -delete
