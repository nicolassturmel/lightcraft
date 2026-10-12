#!/usr/bin/env bash
# Prepare a pinned, checksummed build dependency. The application never fetches a model.
# With a destination argument, also bundle it under <destination>/Models and its licence.
set -euo pipefail
sky_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
sky_cache="${LIGHTCRAFT_APPLE_MODEL_CACHE:-$sky_root/target/apple-models}"
sky_name=DETRResnet50SemanticSegmentationF16P8
"${LIGHTCRAFT_MODEL_PYTHON:-python3}" "$sky_root/packaging/macos/prepare-sky-model.py" \
  "$sky_root/packaging/models/apple-sky.json" "$sky_cache"
sky_stamp="$(cat "$sky_cache/apple-sky-source.sha256")-$(sw_vers -buildVersion)"
if [ ! -d "$sky_cache/$sky_name.mlmodelc" ] || [ "$(cat "$sky_cache/apple-sky-compiled.sha256" 2>/dev/null || true)" != "$sky_stamp" ]; then
  xcrun swift "$sky_root/packaging/macos/compile-sky-model.swift" "$sky_cache/$sky_name.mlpackage" "$sky_cache/$sky_name.mlmodelc"
  printf '%s\n' "$sky_stamp" > "$sky_cache/apple-sky-compiled.sha256"
fi
if [ $# -gt 0 ]; then
  mkdir -p "$1/Models" "$1/Licenses"
  rm -rf "$1/Models/$sky_name.mlmodelc"
  cp -R "$sky_cache/$sky_name.mlmodelc" "$1/Models/"
  cp "$sky_root/assets/models/LICENSE-apple-sky.txt" "$1/Licenses/"
  cp "$sky_root/assets/models/NOTICE-apple-sky.txt" "$1/Licenses/"
fi
