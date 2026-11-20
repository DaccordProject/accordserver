# Build stage
FROM rust:1.88-bookworm AS builder

WORKDIR /app

# Copy manifests first for dependency caching
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

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
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl unzip \
    && rm -rf /var/lib/apt/lists/*
# Microsoft's official Microsoft.ML.OnnxRuntime NuGet package carries the same
# CPU build; the microsoft/onnxruntime GitHub release downloads are gone.
RUN test "$(dpkg --print-architecture)" = amd64 \
    && curl --fail --location --silent --show-error \
      https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/1.22.0/microsoft.ml.onnxruntime.1.22.0.nupkg \
      -o /tmp/runtime.nupkg \
    && echo "d571e63a2329baacb713f441e65ad75284de354db6e1ac435fe4bebbb417986a  /tmp/runtime.nupkg" | sha256sum --check \
    && mkdir -p /opt/onnxruntime/lib \
    && unzip -q -j /tmp/runtime.nupkg 'runtimes/linux-x64/native/*' -d /opt/onnxruntime/lib \
    && unzip -q -j /tmp/runtime.nupkg LICENSE ThirdPartyNotices.txt -d /opt/onnxruntime \
    && rm /tmp/runtime.nupkg

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
