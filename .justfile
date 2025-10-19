test: fmt clippy
  cargo test

fmt:
  cargo fmt --all -- --check

clippy:
  cargo clippy --all-targets --all-features -- -D warnings
