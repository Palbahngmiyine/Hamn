#!/bin/bash
# These are deterministic contract tests; real VM checks run only in release-gate.
set -euo pipefail
unset GITHUB_ACTIONS GITHUB_REPOSITORY GITHUB_RUN_ID GITHUB_RUN_ATTEMPT
[ -x "${HAMN_DEV:-}" ] || { echo 'FAIL: HAMN_DEV must name the built hamn-dev' >&2; exit 2; }
bash -n packaging/release/release-gate.sh
"$HAMN_DEV" test release-physical
if env -u RELEASE_REF -u RELEASE_TAG -u CANDIDATE_DIR -u OUTPUT_DIR \
    bash packaging/release/release-gate.sh > /dev/null 2>&1; then
    echo 'FAIL: physical gate accepted missing inputs' >&2
    exit 1
fi
if env -u HAMN_DEV RELEASE_REF=HEAD RELEASE_TAG=v0.0.1-rc.1 CANDIDATE_DIR=/nonexistent \
    OUTPUT_DIR=/nonexistent bash packaging/release/release-gate.sh > /dev/null 2>&1; then
    echo 'FAIL: physical gate ran without its harness' >&2
    exit 1
fi
echo 'PASS: physical gate input, archive, provenance and runtime contracts'
