#!/usr/bin/env bash
# Smoke-test the actual release image, including dynamic inference dependencies.
set -euo pipefail
image="${1:-accordserver:ci}"
test_dir="$(mktemp -d)"
container="accord-automod-test-$$"
cleanup() {
  docker logs "$container" 2>/dev/null || true
  docker rm -f "$container" >/dev/null 2>&1 || true
  docker run --rm -v "$test_dir:/test-data" "$image" chmod -R a+rwX /test-data >/dev/null 2>&1 || true
  rm -rf "$test_dir"
}
trap cleanup EXIT
mkdir -p "$test_dir/automod-model"
curl --fail --location --silent --show-error \
  https://raw.githubusercontent.com/notAI-tech/NudeNet/v3/nudenet/320n.onnx \
  -o "$test_dir/automod-model/320n.onnx"
echo "c15d8273adad2d0a92f014cc69ab2d6c311a06777a55545f2c4eb46f51911f0f  $test_dir/automod-model/320n.onnx" | sha256sum --check
docker run --rm "$image" ffmpeg -v error -f lavfi -i color=size=320x320 -frames:v 1 -f null -
docker run --rm "$image" ffprobe -version
export ACCORD_BOOTSTRAP_PASSWORD="$(openssl rand -hex 24)"
docker run --rm -v "$test_dir:/app/data" -e ACCORD_BOOTSTRAP_PASSWORD \
  "$image" ./accordserver --bootstrap-admin docker-test
docker run -d --name "$container" -v "$test_dir:/app/data" \
  -e ACCORD_AUTOMOD_SCANNER=local -p 127.0.0.1::39099 "$image"
port="$(docker port "$container" 39099/tcp | cut -d: -f2)"
url="http://127.0.0.1:$port"
curl --fail --silent --show-error --retry 30 --retry-all-errors --retry-delay 1 --max-time 2 "$url/health"
token="$(jq -n --arg password "$ACCORD_BOOTSTRAP_PASSWORD" \
  '{username:"docker-test",password:$password}' | \
  curl --fail --silent --show-error -H 'Content-Type: application/json' \
    --data-binary @- "$url/api/v1/auth/login" | jq -er '.data.token')"
unset ACCORD_BOOTSTRAP_PASSWORD
curl --fail --silent --show-error -H "Authorization: Bearer $token" \
  "$url/api/v1/automod/health" | jq -e '.data.scanner == "ready"'
