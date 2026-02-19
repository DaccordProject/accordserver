# Build stage
FROM rust:1.85-bookworm AS builder

WORKDIR /app

# Copy manifests first for dependency caching
COPY Cargo.toml Cargo.lock ./

# Create a dummy main to build dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs && echo "" > src/lib.rs
COPY build.rs ./
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release && rm -rf src

# Copy the real source code and migrations
COPY src/ src/
COPY migrations/ migrations/

# Pass git SHA as a build arg since .git is not copied
ARG GIT_SHA=unknown
ENV GIT_SHA=${GIT_SHA}

# Touch files so cargo rebuilds the actual source
RUN touch src/main.rs src/lib.rs
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release && cp /app/target/release/accordserver /app/accordserver

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libsqlite3-0 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/accordserver ./
COPY migrations/ migrations/

ENV PORT=39099
ENV DATABASE_URL=sqlite:accord.db?mode=rwc
ENV RUST_LOG=accordserver=debug,tower_http=debug

EXPOSE 39099

CMD ["./accordserver"]
