#!/bin/bash
set -euo pipefail

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

dry_run=${HAMN_CI_DRY_RUN:-0}
case "$dry_run" in
    0|1) ;;
    *) fail "HAMN_CI_DRY_RUN must be 0 or 1" ;;
esac

runner_os=${HAMN_CI_OS:-${RUNNER_OS:-}}
if [ -z "$runner_os" ]; then
    runner_os=$(uname -s)
fi

case "$runner_os" in
    Linux)
        packages=(
            build-essential
            coreutils
            curl
            git
            jq
            openssh-client
            python3
            ripgrep
            ruby
        )
        if [ "$dry_run" -eq 1 ]; then
            printf 'os=Linux packages=%s\n' "${packages[*]}"
            exit 0
        fi
        sudo apt-get update
        sudo apt-get install --yes --no-install-recommends "${packages[@]}"
        required_commands=(bash cc curl git jq make python3 rg ruby sha256sum)
        ;;
    macOS|Darwin)
        formulae=(jq actionlint ripgrep)
        if [ "$dry_run" -eq 1 ]; then
            printf 'os=macOS formulae=%s\n' "${formulae[*]}"
            exit 0
        fi
        command -v brew >/dev/null 2>&1 ||
            fail "Homebrew is required to install macOS test dependencies"
        missing_formulae=()
        for formula in "${formulae[@]}"; do
            command -v "$formula" >/dev/null 2>&1 ||
                missing_formulae+=("$formula")
        done
        if [ "${#missing_formulae[@]}" -gt 0 ]; then
            export HOMEBREW_NO_ANALYTICS=1
            export HOMEBREW_NO_AUTO_UPDATE=1
            export HOMEBREW_NO_INSTALL_CLEANUP=1
            brew install "${missing_formulae[@]}"
        fi
        required_commands=(
            actionlint bash clang codesign git jq make python3 rg ruby shasum
        )
        ;;
    *)
        fail "unsupported CI operating system: $runner_os"
        ;;
esac

for command_name in "${required_commands[@]}"; do
    command -v "$command_name" >/dev/null 2>&1 ||
        fail "required test command is missing: $command_name"
done

jq --version
