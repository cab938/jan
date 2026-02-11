#!/usr/bin/env bash
#
# Clean dev launcher to prevent Snap GLIBC/libpthread issues during `yarn dev`.
# Unsets SNAP/LD_* and then runs the standard dev:tauri pipeline.

set -euo pipefail

echo "[dev-clean] Sanitizing environment..."
for var in SNAP SNAP_NAME SNAP_REVISION SNAP_ARCH SNAP_LIBRARY_PATH LD_LIBRARY_PATH LD_PRELOAD; do
  unset "$var" 2>/dev/null || true
done

echo "[dev-clean] After sanitize:"
env | egrep '^(SNAP|LD_LIBRARY_PATH|LD_PRELOAD)=' || true
echo

# Run the same steps as package.json:dev, then invoke Tauri
yarn build:icon
yarn copy:assets:tauri

echo "[dev-clean] Launching tauri dev..."
exec env IS_CLEAN=true yarn tauri dev