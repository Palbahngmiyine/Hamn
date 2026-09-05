#!/bin/bash
# Compatibility entry point for the physical validator script path.
set -euo pipefail
exec python3 "$(dirname "$0")/physical-e2e.py" "$@"
