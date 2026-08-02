# justfile for blubat - Bluetooth battery monitor for macOS

# Show available recipes
default:
    @just --list

# Build the workspace in debug mode
build:
    cargo build --workspace

# Build the workspace in release mode
release:
    cargo build --release

# Run all tests
test:
    cargo test --workspace

# Lint with clippy, warnings are errors
lint:
    cargo clippy --all-targets -- -D warnings

# Format code with rustfmt
fmt:
    cargo fmt

# Fail if code is not formatted
fmt-check:
    cargo fmt --check

# Assert the core crate stays usable by non-terminal frontends
#
# An allowlist rather than a denylist: every dependency the core takes on has to
# be added here deliberately, so a terminal, notifier or spawner crate cannot
# reach it by being one nobody thought to ban
check-core-isolation:
    #!/usr/bin/env bash
    set -euo pipefail
    allowed="etcetera objc2-core-foundation objc2-io-kit serde serde_json toml"
    actual=$(cargo tree -p blubat-core --edges normal --depth 1 --prefix none \
        | awk 'NR > 1 && NF { print $1 }' | sort -u | tr '\n' ' ' | sed 's/ $//')
    if [ "$actual" != "$allowed" ]; then
        echo "blubat-core's direct dependencies changed" >&2
        echo "  allowed: $allowed" >&2
        echo "  actual:  $actual" >&2
        echo "Add the new crate to this recipe only if it belongs in a crate no frontend can avoid" >&2
        exit 1
    fi
    echo "blubat-core depends on nothing beyond: $allowed"

# The single definition of CI: the GitHub workflow runs this recipe, so a green
# run here is a green pipeline
ci: fmt-check lint test check-core-isolation build
