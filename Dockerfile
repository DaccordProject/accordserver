# Build stage
FROM rust:1.88-bookworm AS builder

WORKDIR /app

# Copy manifests first for dependency caching
COPY Cargo.toml Cargo.lock ./

# Create dummy sources to build dependencies
RUN mkdir -p src/bin && echo "fn main() {}" > src/main.rs && echo "" > src/lib.rs && echo "fn main() {}" > src/bin/seed.rs && echo "fn main() {}" > src/bin/migrate_to_postgres.rs
COPY build.rs ./
RUN cargo build --locked --release && rm -rf src

# Copy the real source code and migrations
COPY src/ src/
COPY migrations/ migrations/

# Pass git SHA as a build arg since .git is not copied
ARG GIT_SHA=unknown
ENV GIT_SHA=${GIT_SHA}

# Build the real binaries (no caching to ensure version is always correct)
RUN cargo build --locked --release && cp target/release/accordserver /app/accordserver && cp target/release/accord-seed /app/accord-seed

# Match ort's pinned 1.22 API. Keep download tools out of the final image.
# The published image currently targets Linux amd64.
FROM debian:bookworm-slim AS automod-runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
RUN test "$(dpkg --print-architecture)" = amd64 \
    && curl --fail --location --silent --show-error \
      https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-linux-x64-1.22.0.tgz \
      -o /tmp/runtime.tgz \
    && echo "8344d55f93d5bc5021ce342db50f62079daf39aaafb5d311a451846228be49b3  /tmp/runtime.tgz" | sha256sum --check \
    && mkdir -p /opt/onnxruntime \
    && tar -xzf /tmp/runtime.tgz --strip-components=1 -C /opt/onnxruntime \
    && rm /tmp/runtime.tgz

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    ffmpeg \
    libssl3 \
    libsqlite3-0 \
    libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/accordserver ./
COPY --from=builder /app/accord-seed ./
# Include the runtime's license and third-party notices alongside its libraries.
COPY --from=automod-runtime /opt/onnxruntime /opt/onnxruntime
COPY migrations/ migrations/
RUN mkdir -p /app/data

ENV PORT=39099
ENV DATABASE_URL=sqlite:/app/data/accord.db?mode=rwc
ENV RUST_LOG=accordserver=debug,tower_http=debug
ENV ACCORD_AUTOMOD_RUNTIME_PATH=/opt/onnxruntime/lib/libonnxruntime.so
ENV ACCORD_AUTOMOD_MODEL_PATH=/app/data/automod-model/320n.onnx

EXPOSE 39099

CMD ["./accordserver"]
