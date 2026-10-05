# Hebnix Linux Makefile
#
# Canonical build/install interface. GitHub Actions, install.sh, and the
# AUR PKGBUILDs all call into this file rather than each keeping their own
# build recipe -- `make release` is the one build path; `make install`
# (with PREFIX/DESTDIR overridden as needed) is the one install path.

.PHONY: all build release debug clean install uninstall run test check fmt clippy moon-bundle help

# Default target
all: release

# Build release version
build: release

release:
	@echo "Building release version..."
	cargo build --release

# Build debug version
debug:
	@echo "Building debug version..."
	cargo build

# Clean build artifacts
clean:
	@echo "Cleaning build artifacts..."
	cargo clean

# Installed command name (public interface); the Cargo binary itself is
# still called hebnix-app internally (see crates/hebnix-app/Cargo.toml).
BIN_NAME := hebnix
CARGO_BIN := hebnix-app

# Conventional packaging variables. Defaults give the old no-sudo
# ~/.local/bin behavior; a system package build overrides both, e.g.:
#   make PREFIX=/usr DESTDIR="$pkgdir" install
PREFIX ?= $(HOME)/.local
DESTDIR ?=

BINDIR := $(DESTDIR)$(PREFIX)/bin
APPDIR := $(DESTDIR)$(PREFIX)/share/applications
ICONDIR := $(DESTDIR)$(PREFIX)/share/icons/hicolor/256x256/apps

# Install the binary, .desktop file, and icon under PREFIX (DESTDIR-aware).
# No sudo, no privileged paths touched here -- the caller decides PREFIX.
install: release
	@echo "Installing $(BIN_NAME) to $(BINDIR)..."
	install -Dm755 target/release/$(CARGO_BIN) "$(BINDIR)/$(BIN_NAME)"
	install -Dm644 packaging/hebnix.desktop "$(APPDIR)/hebnix.desktop"
	install -Dm644 crates/hebnix-app/assets/hebnix.png "$(ICONDIR)/hebnix.png"
	@echo "Installation complete. Run with: $(BIN_NAME) (make sure $(PREFIX)/bin is on your PATH)"

uninstall:
	@echo "Uninstalling $(BIN_NAME)..."
	rm -f "$(BINDIR)/$(BIN_NAME)"
	rm -f "$(APPDIR)/hebnix.desktop"
	rm -f "$(ICONDIR)/hebnix.png"
	@echo "Uninstall complete."

# Run the release binary
run: release
	@echo "Running hebnix-app..."
	./target/release/hebnix-app

# Run tests
test:
	@echo "Running tests..."
	cargo test

# Check code without building
check:
	@echo "Checking code..."
	cargo check

# Format code
fmt:
	@echo "Formatting code..."
	cargo fmt

# Run clippy linter
clippy:
	@echo "Running clippy..."
	cargo clippy -- -D warnings

# Development build and run
dev: debug
	@echo "Running debug version..."
	./target/debug/hebnix-app

# Package the built release as a single .moon bundle ( packaging/moon ).
moon-bundle: release
	@echo "Building .moon bundle..."
	bash packaging/moon/build-moon-bundle.sh --binary target/release/$(CARGO_BIN)

# Show help
help:
	@echo "Hebnix Linux - Available targets:"
	@echo ""
	@echo "  make              - Build release version (default)"
	@echo "  make release      - Build optimized release version"
	@echo "  make debug        - Build debug version"
	@echo "  make clean        - Remove build artifacts"
	@echo "  make install      - Install to \$$PREFIX (default ~/.local, no sudo needed)"
	@echo "  make uninstall    - Remove from \$$PREFIX"
	@echo "                      e.g. system packaging: make PREFIX=/usr DESTDIR=\"\$$pkgdir\" install"
	@echo "  make run          - Build and run release version"
	@echo "  make dev          - Build and run debug version"
	@echo "  make test         - Run tests"
	@echo "  make check        - Check code without building"
	@echo "  make fmt          - Format code with rustfmt"
	@echo "  make clippy       - Run clippy linter"
	@echo "  make moon-bundle  - Package the release as one .moon bundle"
	@echo "  make help         - Show this help message"
