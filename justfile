# justfile for blubat - Bluetooth battery monitor for macOS

# Show available recipes
default:
    @just --list

# Build the workspace in debug mode
build:
    cargo build

# Build the workspace in release mode
release:
    cargo build --release

# Run all tests
test:
    cargo test

# Lint with clippy, warnings are errors
lint:
    cargo clippy --all-targets -- -D warnings

# Format code with rustfmt
fmt:
    cargo fmt

# Fail if code is not formatted
fmt-check:
    cargo fmt -- --check

# Assert the core crate stays usable by non-terminal frontends
check-core-isolation:
    #!/usr/bin/env bash
    set -euo pipefail
    if cargo tree -p blubat-core --edges normal | grep -qE '\b(ratatui|crossterm)\b'; then
        echo "blubat-core must not depend on ratatui or crossterm" >&2
        exit 1
    fi
    echo "blubat-core is free of terminal dependencies"

# Everything CI runs
ci: fmt-check lint test check-core-isolation build
