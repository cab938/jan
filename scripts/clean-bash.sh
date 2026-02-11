#!/usr/bin/env bash
#
# Clean Bash launcher for VS Code Insiders (Snap) to avoid Snap GLIBC/libpthread
# injected env. Unsets SNAP/LD_* vars and starts a login, interactive Bash.

set -euo pipefail

# Unset Snap-related and dynamic loader vars
unset SNAP SNAP_NAME SNAP_REVISION SNAP_ARCH SNAP_LIBRARY_PATH || true
unset LD_LIBRARY_PATH LD_PRELOAD || true

# Prefer system PATH first; keep existing PATH at the end
export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:${PATH:-}"

echo "[Clean Bash] SNAP/LD_* variables unset. Starting clean login shell..."

# Start a login, interactive Bash
exec -l /bin/bash -i