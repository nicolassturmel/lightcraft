#!/usr/bin/env bash
# Prepare a pinned, checksummed build dependency. The application never fetches a model.
# With a destination argument, also bundle it under <destination>/Models and its licence.
set -euo pipefail
inpaint_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
inpaint_cache="${LIGHTCRAFT_APPLE_MODEL_CACHE:-$inpaint_root/target/apple-models}"
inpaint_name=LaMa
"${LIGHTCRAFT_MODEL_PYTHON:-python3}" "$inpaint_root/packaging/macos/prepare-sky-model.py" \
  "$inpaint_root/packaging/models/apple-inpaint.json" "$inpaint_cache"
inpaint_stamp="$(cat "$inpaint_cache/apple-inpaint-source.sha256")-$(sw_vers -buildVersion)"
if [ ! -d "$inpaint_cache/$inpaint_name.mlmodelc" ] || [ "$(cat "$inpaint_cache/apple-inpaint-compiled.sha256" 2>/dev/null || true)" != "$inpaint_stamp" ]; then
  xcrun swift "$inpaint_root/packaging/macos/compile-inpaint-model.swift" "$inpaint_cache/$inpaint_name.mlpackage" "$inpaint_cache/$inpaint_name.mlmodelc"
  printf '%s\n' "$inpaint_stamp" > "$inpaint_cache/apple-inpaint-compiled.sha256"
fi
if [ $# -gt 0 ]; then
  mkdir -p "$1/Models" "$1/Licenses"
  rm -rf "$1/Models/$inpaint_name.mlmodelc"
  cp -R "$inpaint_cache/$inpaint_name.mlmodelc" "$1/Models/"
  cp "$inpaint_root/assets/models/LICENSE-lama.txt" "$1/Licenses/"
  cp "$inpaint_root/assets/models/NOTICE-lama.txt" "$1/Licenses/"
fi
