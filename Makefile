CARGO  ?= cargo
PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin

BIN     = rend
TUI_BIN = rend-tui

RELEASE_DIR = target/release
DEBUG_DIR   = target/debug

.DEFAULT_GOAL := all

.PHONY: all debug release install install-tui uninstall test clippy fmt lint clean help

all: debug

## Build the debug binary
debug:
	$(CARGO) build

## Build the release binary
release:
	$(CARGO) build --release

## Install the release binary into $(BINDIR)
install: release
	install -d $(BINDIR)
	install -m 0755 $(RELEASE_DIR)/$(BIN) $(BINDIR)/$(BIN)

## Install the release TUI binary into $(BINDIR)
install-tui: release
	install -d $(BINDIR)
	install -m 0755 $(RELEASE_DIR)/$(TUI_BIN) $(BINDIR)/$(TUI_BIN)

## Remove installed binaries from $(BINDIR)
uninstall:
	rm -f $(BINDIR)/$(BIN) $(BINDIR)/$(TUI_BIN)

## Run the test suite
test:
	$(CARGO) test --workspace

## Run clippy lints (denying warnings, as in CI)
clippy:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

## Format the workspace
fmt:
	$(CARGO) fmt --all

## Check formatting and lints without modifying files
lint:
	$(CARGO) fmt --all --check
	$(CARGO) clippy --workspace --all-targets -- -D warnings

## Remove all build artifacts
clean:
	$(CARGO) clean

## Show this help
help:
	@echo "Usage: make [target] [PREFIX=/path]"
	@echo
	@echo "Targets:"
	@echo "  all          Build the debug binary (default)"
	@echo "  debug        Build the debug binary"
	@echo "  release      Build the release binary"
	@echo "  install      Build and install '$(BIN)' to $(PREFIX)/bin"
	@echo "  install-tui  Build and install '$(TUI_BIN)' to $(PREFIX)/bin"
	@echo "  uninstall    Remove installed binaries from $(PREFIX)/bin"
	@echo "  test         Run the test suite"
	@echo "  clippy       Run clippy lints"
	@echo "  fmt          Format the workspace"
	@echo "  lint         Check formatting and lints"
	@echo "  clean        Remove all build artifacts"
	@echo
	@echo "Variables:"
	@echo "  PREFIX  Install prefix (default: ~/.local)"
	@echo "  BINDIR  Binary directory (default: \$$PREFIX/bin)"
	@echo "  CARGO   Cargo executable (default: cargo)"
