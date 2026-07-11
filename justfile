default: check

check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

fmt:
    cargo fmt --all

fixtures:
    ./fixtures/generate.sh

# Fetch + patch + build the vendored analysis decoders.
vendor:
    ./vendor/build-decoders.sh

# Assemble target/StreamLens.app (macOS Dock name + icon).
bundle:
    cargo build --release -p streamlens
    ./packaging/macos-app.sh

run *ARGS:
    cargo run -p streamlens -- {{ARGS}}

bench:
    cargo build --release -p streamlens-core --example play_bench
    cargo test --release -p streamlens-core --test playback_smoothness -- --ignored --nocapture
