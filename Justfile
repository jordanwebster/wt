verify: lint test failpoints fmt-check

lint:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

failpoints:
    cargo test --workspace --features failpoints

fmt-check:
    cargo fmt --all -- --check
