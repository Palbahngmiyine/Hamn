#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# Test tools come only from flake.nix shells; package-manager setup must not return.
for removed in scripts/ci/setup-test-dependencies.sh \
    scripts/ci/use-system-macos-sdk.sh; do
    [ ! -e "$ROOT/$removed" ] || fail "non-Nix test environment setup remains: $removed"
done
if grep -Eq '(^|[^[:alnum:]_])(brew|apt-get|rustup|swift|xcodebuild)([^[:alnum:]_]|$)' \
    "$ROOT/flake.nix" >/dev/null; then
    fail "Nix test shells depend on a package manager, rustup, or a removed Desktop build tool"
fi

while IFS= read -r script; do
    [ -f "$ROOT/$script" ] || continue
    bash -n "$ROOT/$script"
done < <(git -C "$ROOT" ls-files '*.sh' | LC_ALL=C sort)

ruby -e 'require "yaml"; ARGV.each { |path| Psych.parse_file(path) }' \
    "$ROOT/.github/actionlint.yaml" "$ROOT"/.github/workflows/*.yml

if git -C "$ROOT" ls-files --error-unmatch 'desktop/*' >/dev/null 2>&1; then
    fail "tracked Desktop source remains"
fi
for removed in packaging/homebrew \
    packaging/release/verify-macos-release.sh \
    tests/ci/test_desktop_xcode.sh; do
    [ ! -e "$ROOT/$removed" ] || fail "removed Desktop asset remains: $removed"
done

bash "$ROOT/tests/host/test_nested_virtualization_contract.sh"

bash "$ROOT/guest/tests/test_configure_containerd.sh"
bash "$ROOT/guest/tests/test_configure_docker.sh"
bash "$ROOT/guest/tests/test_configure_rosetta.sh"
bash "$ROOT/guest/tests/test_make_install_targets.sh"
bash "$ROOT/guest/tests/test_guest_deployment_transaction.sh"
bash "$ROOT/guest/tests/test_verify_image_contract.sh"
bash "$ROOT/guest/tests/test_guest_image_builder.sh"
git -C "$ROOT" diff --check

echo "PASS: CLI-only portable test gates"
