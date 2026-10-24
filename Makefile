CARGO  ?= cargo
DOCKER ?= docker
PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin

BIN     = rend
TUI_BIN = rend-tui
WUI_BIN = rend-wui

RELEASE_DIR = target/release
DEBUG_DIR   = target/debug

# Local dev image: host platform, loaded into the daemon.
IMAGE = $(WUI_BIN):local
# Registry image: the tag comes from git describe (vX.Y.Z when HEAD is tagged,
# the short SHA otherwise) and can be overridden with TAG=.
IMAGE_NAME_NO_VER ?= cthelight/rend-wui
TAG ?= $(shell git describe --tags --always 2>/dev/null || echo dev)
IMAGE_NAME := $(IMAGE_NAME_NO_VER):$(TAG)
# Platforms for multi-arch builds (override as needed). amd64 and arm64/v8
# only: the builder stage's rust:alpine base image ships no 32-bit ARM.
PLATFORMS ?= linux/amd64,linux/arm64/v8

.DEFAULT_GOAL := all

.PHONY: all debug release install install-cli install-tui install-wui uninstall test clippy fmt lint clean docker docker_tag docker-demo push multiarch_push help

all: debug

## Build the debug binary
debug:
	$(CARGO) build

## Build the release binary
release:
	$(CARGO) build --release

## Install all binaries into $(BINDIR)
install: install-cli install-tui install-wui

## Install the release CLI binary into $(BINDIR)
install-cli: release
	install -d $(BINDIR)
	install -m 0755 $(RELEASE_DIR)/$(BIN) $(BINDIR)/$(BIN)

## Install the release TUI binary into $(BINDIR)
install-tui: release
	install -d $(BINDIR)
	install -m 0755 $(RELEASE_DIR)/$(TUI_BIN) $(BINDIR)/$(TUI_BIN)

## Install the release web UI binary into $(BINDIR)
install-wui: release
	install -d $(BINDIR)
	install -m 0755 $(RELEASE_DIR)/$(WUI_BIN) $(BINDIR)/$(WUI_BIN)

## Remove installed binaries from $(BINDIR)
uninstall:
	rm -f $(BINDIR)/$(BIN) $(BINDIR)/$(TUI_BIN) $(BINDIR)/$(WUI_BIN)

## Build the minimal docker image (static rend-wui + audio-only ffmpeg)
docker:
	$(DOCKER) buildx build --load -t $(IMAGE) .

## Build the image and run it with the simulated disc on port 3000
docker-demo: docker
	$(DOCKER) run --rm -p 3000:3000 $(IMAGE) --demo

## Build the host-platform image tagged with the git version
docker_tag:
	$(DOCKER) buildx build --load -t $(IMAGE_NAME) .

## Push the host-platform (single-arch) image to the registry
push:
	@echo -n "Push $(IMAGE_NAME)? [y/N] " && read ans && if ! [ $${ans:-'N'} = 'y' ]; then 1>&2 echo "Aborting..."; exit 1; fi
	$(DOCKER) buildx build --tag $(IMAGE_NAME) --push .

# Multi-arch images cannot be loaded into a local docker daemon, so this
# builds all platforms and pushes the result to the registry.
multiarch_push:
	@echo -n "Push $(IMAGE_NAME) for $(PLATFORMS)? [y/N] " && read ans && if ! [ $${ans:-'N'} = 'y' ]; then 1>&2 echo "Aborting..."; exit 1; fi
	$(DOCKER) buildx build --platform $(PLATFORMS) --tag $(IMAGE_NAME) --push .

## Run the test suite
test:
	$(CARGO) test --workspace

## Run clippy lints (denying warnings)
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
	@echo "  all          Build the debug binaries (default)"
	@echo "  debug        Build the debug binaries"
	@echo "  release      Build the release binaries"
	@echo "  install      Build and install '$(BIN)', '$(TUI_BIN)' and '$(WUI_BIN)' to $(PREFIX)/bin"
	@echo "  install-cli  Build and install '$(BIN)' to $(PREFIX)/bin"
	@echo "  install-tui  Build and install '$(TUI_BIN)' to $(PREFIX)/bin"
	@echo "  install-wui  Build and install '$(WUI_BIN)' to $(PREFIX)/bin"
	@echo "  uninstall    Remove installed binaries from $(PREFIX)/bin"
	@echo "  docker       Build the minimal docker image ($(IMAGE))"
	@echo "  docker-demo  Build and run the image with the simulated disc"
	@echo "  docker_tag   Build the host-platform image tagged with the git version"
	@echo "  push         Push the host-platform image ($(IMAGE_NAME)) to the registry"
	@echo "  multiarch_push  Push a multi-arch image for $(PLATFORMS)"
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
	@echo "  DOCKER  Docker executable (default: docker)"
	@echo "  TAG     Image tag for the push targets (default: git describe)"
	@echo "  PLATFORMS  Platforms for multiarch_push"
