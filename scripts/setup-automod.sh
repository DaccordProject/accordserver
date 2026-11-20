#!/usr/bin/env bash
# Install the verified NudeNet 320n model and ONNX Runtime 1.22 CPU library.
# The runtime package below is for Linux x86_64 and needs unzip. See docs/automod.md for others.
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
# Microsoft's official NuGet package; the GitHub release downloads are gone.
curl --fail --location --silent --show-error \
  https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/1.22.0/microsoft.ml.onnxruntime.1.22.0.nupkg \
  -o "$staging_dir/runtime.nupkg"
(
  cd "$staging_dir"
  sha256sum --check <<'CHECKSUMS'
c15d8273adad2d0a92f014cc69ab2d6c311a06777a55545f2c4eb46f51911f0f  320n.onnx
d571e63a2329baacb713f441e65ad75284de354db6e1ac435fe4bebbb417986a  runtime.nupkg
CHECKSUMS
)
runtime_dir="$model_dir/onnxruntime-linux-x64-1.22.0"
mkdir -p "$runtime_dir/lib"
unzip -q -o -j "$staging_dir/runtime.nupkg" 'runtimes/linux-x64/native/*' -d "$runtime_dir/lib"
unzip -q -o -j "$staging_dir/runtime.nupkg" LICENSE ThirdPartyNotices.txt -d "$runtime_dir"
cp "$staging_dir/320n.onnx" "$model_dir/320n.onnx"
printf '\nSet these variables when launching accordserver, then enable a policy:\n'
printf 'export ACCORD_AUTOMOD_SCANNER=local\n'
printf 'export ACCORD_AUTOMOD_MODEL_PATH=%q\n' "$model_dir/320n.onnx"
printf 'export ACCORD_AUTOMOD_RUNTIME_PATH=%q\n' "$model_dir/onnxruntime-linux-x64-1.22.0/lib/libonnxruntime.so"

printf '\nVideo sampling requires ffmpeg and ffprobe on PATH, or their ACCORD_AUTOMOD_*_PATH settings.\n'
