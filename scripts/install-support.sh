#!/bin/bash
# Resolve support from this release payload, installed generation, or completed
# developer build. Never search PATH or use the BINDIR/hamn being replaced.
if [ -f "$ROOT/Cargo.toml" ]; then
    INSTALL_SUPPORT=$ROOT/build/hamn
elif [[ "$ROOT" =~ /\.hamn-generations/[0-9a-f]{64}-[A-Za-z0-9]{6}/share/hamn/src$ ]]; then
    INSTALL_SUPPORT=$ROOT/../../../bin/hamn
else
    INSTALL_SUPPORT=$ROOT/bin/hamn
fi
if [ ! -f "$INSTALL_SUPPORT" ] || [ -L "$INSTALL_SUPPORT" ] || [ ! -x "$INSTALL_SUPPORT" ]; then
    echo "hamn: installer release support executable is missing" >&2
    exit 1
fi
install_support() { "$INSTALL_SUPPORT" __install-support "$@"; }
