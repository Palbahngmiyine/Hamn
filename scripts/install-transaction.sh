#!/bin/bash
# Sourced by install/update with BINDIR and DATADIR. Hold both root locks until
# process exit, including recovery and generation collection. Children inherit
# descriptors, not permission to bypass locks; every use checks their identity.
if [ -L "$BINDIR" ] || [ -L "$DATADIR" ]; then
    echo "hamn: refusing symlinked transaction root" >&2
    exit 1
fi
mkdir -p "$BINDIR" "$(dirname "$DATADIR")"
BINDIR=$(cd "$BINDIR" && pwd -P)
DATADIR=$(cd "$(dirname "$DATADIR")" && pwd -P)/$(basename "$DATADIR")
transaction_lock() {
    local path=$1 fd=$2
    install_support lock-prepare "$path"
    # A nested installer can reuse an inherited descriptor only for this inode.
    if ! install_support lock-same "$path" "$fd" 2>/dev/null; then
        if [ "$fd" = 6 ]; then exec 6>>"$path"; else exec 7>>"$path"; fi
    fi
    install_support lock-acquire "$path" "$fd"

}
transaction_one=$BINDIR/.hamn-transaction.lock
transaction_two=$(dirname "$DATADIR")/.$(basename "$DATADIR").hamn-transaction.lock
if [[ "$transaction_two" < "$transaction_one" ]]; then
    transaction_swap=$transaction_one
    transaction_one=$transaction_two
    transaction_two=$transaction_swap
fi
transaction_lock "$transaction_one" 6
transaction_lock "$transaction_two" 7
