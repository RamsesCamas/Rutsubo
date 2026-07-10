FROM rust:1.92-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p rutsubo-daemon

FROM debian:bookworm-slim
RUN useradd --create-home --uid 10001 rutsubo
COPY --from=build /app/target/release/rutsubo-daemon /usr/local/bin/rutsubo-daemon
USER rutsubo
ENTRYPOINT ["/usr/local/bin/rutsubo-daemon"]
