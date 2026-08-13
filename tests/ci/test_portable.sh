#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
SETUP="$ROOT/scripts/ci/setup-test-dependencies.sh"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

linux_plan=$(HAMN_CI_OS=Linux HAMN_CI_DRY_RUN=1 bash "$SETUP")
case "$linux_plan" in
"os=Linux packages="*) ;;
*) fail "unexpected Linux dependency plan: $linux_plan" ;;
esac
linux_packages=${linux_plan#os=Linux packages=}
for package in build-essential coreutils curl git jq openssh-client python3 ripgrep ruby; do
    case " $linux_packages " in
    *" $package "*) ;;
    *) fail "Linux dependency plan omits $package: $linux_plan" ;;
    esac
done

macos_plan=$(HAMN_CI_OS=macOS HAMN_CI_DRY_RUN=1 bash "$SETUP")
[ "$macos_plan" = 'os=macOS formulae=jq actionlint ripgrep' ] ||
    fail "unexpected macOS dependency plan: $macos_plan"
if grep -Eq '(^|[^[:alnum:]_])(swift|xcrun)([^[:alnum:]_]|$)' "$SETUP" >/dev/null; then
    fail "CLI-only CI setup still requires a removed Desktop build tool"
fi

if HAMN_CI_OS=Plan9 HAMN_CI_DRY_RUN=1 bash "$SETUP" \
    >"$WORK/unsupported.out" 2>"$WORK/unsupported.err"; then
    fail "dependency setup accepted an unsupported operating system"
fi
grep -Fq 'unsupported CI operating system: Plan9' "$WORK/unsupported.err"

if HAMN_CI_OS=Linux HAMN_CI_DRY_RUN=invalid bash "$SETUP" \
    >"$WORK/dry-run.out" 2>"$WORK/dry-run.err"; then
    fail "dependency setup accepted an invalid dry-run value"
fi
grep -Fq 'HAMN_CI_DRY_RUN must be 0 or 1' "$WORK/dry-run.err"

while IFS= read -r script; do
    [ -f "$ROOT/$script" ] || continue
    bash -n "$ROOT/$script"
done < <(git -C "$ROOT" ls-files '*.sh' | LC_ALL=C sort)

ruby -e 'require "yaml"; ARGV.each { |path| Psych.parse_file(path) }' \
    "$ROOT/.github/actionlint.yaml" "$ROOT/.github/workflows/release.yml"

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
bash "$ROOT/guest/tests/test_k3s_configuration.sh"
bash "$ROOT/guest/tests/test_guest_deployment_transaction.sh"
bash "$ROOT/guest/tests/test_verify_image_contract.sh"
bash "$ROOT/guest/tests/test_guest_image_builder.sh"
bash "$ROOT/guest/tests/test_install_k3s.sh"
git -C "$ROOT" diff --check

echo "PASS: CLI-only portable test gates"
