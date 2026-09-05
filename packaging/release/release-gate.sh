#!/bin/bash
# Validate exact RC bytes on a physical Apple Silicon runner. Never rebuild.
set -euo pipefail
export LC_ALL=C
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
fail() { echo "hamn release gate: $*" >&2; exit 1; }
[ -n "${RELEASE_REF:-}" ] && [ -n "${RELEASE_TAG:-}" ] && \
    [ -n "${CANDIDATE_DIR:-}" ] && [ -n "${OUTPUT_DIR:-}" ] || \
    fail 'RELEASE_REF, RELEASE_TAG, CANDIDATE_DIR, and OUTPUT_DIR are required'
[ "$(uname -m)" = arm64 ] || fail 'physical Apple Silicon validator required'
[ -z "$(git -C "$ROOT" status --porcelain)" ] || fail 'validator source tree is dirty'
commit=$(git -C "$ROOT" rev-parse --verify "$RELEASE_REF^{commit}")
[ "$commit" = "$(git -C "$ROOT" rev-parse HEAD)" ] || fail 'release commit differs from checkout'
tree=$(git -C "$ROOT" rev-parse "$commit^{tree}")
[ -d "$CANDIDATE_DIR" ] && [ ! -L "$CANDIDATE_DIR" ] || fail 'unsafe candidate directory'
python3 - "$ROOT" "$CANDIDATE_DIR" "$RELEASE_TAG" "$commit" "$tree" <<'CHECK'
import sys
sys.path.insert(0, sys.argv[1] + '/packaging/release')
from physical_contract import validate_candidate
validate_candidate(*sys.argv[2:])
CHECK
mkdir -p "$OUTPUT_DIR"
[ -d "$OUTPUT_DIR" ] && [ ! -L "$OUTPUT_DIR" ] || fail 'unsafe output directory'
[ -z "$(find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ] || fail 'output directory must be empty'
work=$(mktemp -d "$OUTPUT_DIR/.gate.XXXXXX")
trap 'rm -rf "$work"' EXIT
harness=$(python3 - "$ROOT" "$CANDIDATE_DIR" "$work" <<'UNPACK'
import importlib.util
from pathlib import Path
import sys
sys.path.insert(0, sys.argv[1] + '/packaging/release')
from physical_contract import read_json
spec = importlib.util.spec_from_file_location('physical', sys.argv[1] + '/packaging/release/physical-e2e.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
directory, destination = Path(sys.argv[2]), Path(sys.argv[3])
candidate = read_json(directory / 'candidate.json')
host = next(item['name'] for item in candidate['artifacts'] if item['name'].endswith('-darwin-arm64.tar.gz'))
print(module.unpack(directory / host, destination) / 'packaging/release/physical-e2e.py')
UNPACK
)
HAMN_CANDIDATE_DIR="$CANDIDATE_DIR" \
HAMN_E2E_OUTPUT="$OUTPUT_DIR/physical-validation-evidence.json" \
    python3 "$harness"
python3 - "$ROOT" "$CANDIDATE_DIR/candidate.json" "$CANDIDATE_DIR/SHA256SUMS" \
    "$OUTPUT_DIR/physical-validation-evidence.json" "${GITHUB_RUN_ID:-local}" "${GITHUB_RUN_ATTEMPT:-local}" <<'CHECK'
import sys
sys.path.insert(0, sys.argv[1] + '/packaging/release')
from physical_contract import validate
validate(*sys.argv[2:])
CHECK
echo "validated exact candidate $RELEASE_TAG; physical evidence is in $OUTPUT_DIR"
