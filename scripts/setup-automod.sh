#!/usr/bin/env bash
# Install the verified NudeNet 320n model and ONNX Runtime 1.22 CPU library.
# The runtime archive below is for Linux x86_64. See docs/automod.md for others.
set -euo pipefail
if [[ "$(uname -s)" != Linux || "$(uname -m)" != x86_64 ]]; then
  echo 'This helper supports Linux x86_64; see docs/automod.md for manual setup.' >&2
  exit 1
fi
model_dir="${1:-./data/automod-model}"
mkdir -p "$model_dir"
model_dir="$(cd "$model_dir" && pwd)"
staging_dir="$(mktemp -d)"
trap 'rm -rf "$staging_dir"' EXIT
curl --fail --location --silent --show-error \
  https://raw.githubusercontent.com/notAI-tech/NudeNet/v3/nudenet/320n.onnx \
  -o "$staging_dir/320n.onnx"
curl --fail --location --silent --show-error \
  https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-linux-x64-1.22.0.tgz \
  -o "$staging_dir/runtime.tgz"
(
  cd "$staging_dir"
  sha256sum --check <<'CHECKSUMS'
c15d8273adad2d0a92f014cc69ab2d6c311a06777a55545f2c4eb46f51911f0f  320n.onnx
8344d55f93d5bc5021ce342db50f62079daf39aaafb5d311a451846228be49b3  runtime.tgz
CHECKSUMS
)
tar -xzf "$staging_dir/runtime.tgz" -C "$staging_dir"
cp "$staging_dir/320n.onnx" "$model_dir/320n.onnx"
cp -R "$staging_dir/onnxruntime-linux-x64-1.22.0" "$model_dir/"
printf '\nSet these variables when launching accordserver, then enable a policy:\n'
printf 'export ACCORD_AUTOMOD_SCANNER=local\n'
printf 'export ACCORD_AUTOMOD_MODEL_PATH=%q\n' "$model_dir/320n.onnx"
printf 'export ACCORD_AUTOMOD_RUNTIME_PATH=%q\n' "$model_dir/onnxruntime-linux-x64-1.22.0/lib/libonnxruntime.so"

printf '\nVideo sampling requires ffmpeg and ffprobe on PATH, or their ACCORD_AUTOMOD_*_PATH settings.\n'
