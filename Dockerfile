# Build stage
FROM rust:1.84-bookworm AS builder

WORKDIR /app

# Copy manifests first for dependency caching
COPY Cargo.toml Cargo.lock ./

# Create a dummy main to build dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs && echo "" > src/lib.rs
RUN cargo build --release && rm -rf src

# Copy the real source code and migrations
COPY src/ src/
COPY migrations/ migrations/

# Touch files so cargo rebuilds the actual source
RUN touch src/main.rs src/lib.rs
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libsqlite3-0 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/accordserver ./
COPY migrations/ migrations/

ENV PORT=39099
ENV DATABASE_URL=sqlite:accord.db?mode=rwc
ENV RUST_LOG=accordserver=debug,tower_http=debug

EXPOSE 39099

CMD ["./accordserver"]
