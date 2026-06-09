# https://just.systems

precommit: format lint dependencies commits test

# Check if code is formatted correctly
format:
    cargo fmt --check
    taplo check

# Run static analysis
lint:
    cargo check
    cargo clippy --all-targets --all-features -- -D warnings

# Check dependencies and licensing
dependencies:
    cargo machete
    cargo deny check
    cargo audit

# Check commit messages
commits:
    committed origin/master..HEAD

# Attempts to automatically fix issues we can
fix:
    cargo clippy --fix --allow-dirty
    cargo fmt
    taplo format

# Generate the CHANGELOG.md from the Git history.
changelog:
    git-cliff -o CHANGELOG.md --latest --strip all

# Runs all unit tests in the workspace.
test:
    cargo nextest run --no-fail-fast

# Install all tools used for this repo's CI and other tools
setup:
    cargo install cargo-deny
    cargo install committed
    cargo install git-cliff
    cargo install --locked cargo-nextest
    cargo install cargo-machete
    cargo install taplo-cli --locked
    cargo install cargo-audit --locked
    cargo fetch --locked
