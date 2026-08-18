#!/bin/bash
# Source inside a Nix shell to build Hamn against the runner's current Apple SDK.
set -euo pipefail

[ "$(uname -s)" = Darwin ] || {
    echo "FAIL: the system macOS SDK is available only on Darwin" >&2
    return 1 2>/dev/null || exit 1
}
[ -x /usr/bin/xcrun ] && [ -x /usr/bin/clang ] && [ -x /usr/bin/codesign ] || {
    echo "FAIL: the Apple command line toolchain is unavailable" >&2
    return 1 2>/dev/null || exit 1
}

if [ -n "${HAMN_SYSTEM_SDKROOT:-}" ]; then
    SDKROOT=$HAMN_SYSTEM_SDKROOT
else
    SDKROOT=$(env -u SDKROOT -u DEVELOPER_DIR \
        /usr/bin/xcrun --sdk macosx --show-sdk-path)
fi
[ -d "$SDKROOT" ] || {
    echo "FAIL: xcrun returned an invalid macOS SDK" >&2
    return 1 2>/dev/null || exit 1
}
case "$SDKROOT" in
/nix/store/*)
    echo "FAIL: Hamn must not compile against the Nix Apple SDK" >&2
    return 1 2>/dev/null || exit 1
    ;;
esac

export SDKROOT
export PATH="/usr/bin:/bin:/usr/sbin:/sbin:$PATH"
[ "$(command -v clang)" = /usr/bin/clang ]
[ "$(command -v codesign)" = /usr/bin/codesign ]
echo "using system macOS SDK: $SDKROOT"
