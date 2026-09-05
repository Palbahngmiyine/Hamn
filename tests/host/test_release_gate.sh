#!/bin/bash
# These are deterministic contract tests; real VM checks run only in release-gate.
set -euo pipefail
unset GITHUB_ACTIONS GITHUB_REPOSITORY GITHUB_RUN_ID GITHUB_RUN_ATTEMPT
bash -n packaging/release/release-gate.sh packaging/release/physical-e2e.sh
python3 tests/host/test_physical_contract.py
python3 tests/host/test_physical_runtime.py
if env -u RELEASE_REF -u RELEASE_TAG -u CANDIDATE_DIR -u OUTPUT_DIR \
    bash packaging/release/release-gate.sh > /dev/null 2>&1; then
    echo 'FAIL: physical gate accepted missing inputs' >&2
    exit 1
fi
echo 'PASS: physical gate input, archive, provenance and preservation contracts'
