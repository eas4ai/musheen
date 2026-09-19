FROM rust:1.95-bookworm

WORKDIR /workspace
COPY . .

RUN cargo fmt --all --check
RUN cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
RUN cargo test --workspace --all-features --locked
RUN cargo build --workspace --all-features --locked
