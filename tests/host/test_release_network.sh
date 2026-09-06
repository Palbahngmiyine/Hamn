#!/bin/bash
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir "$WORK/bin"
awk '
    /# libguestfs auto-selects passt/ { capture = 1 }
    /multiarch=\$\(dpkg-architecture/ { capture = 0 }
    capture { print }
' "$ROOT/.github/workflows/release.yml" > "$WORK/network.sh"
test -s "$WORK/network.sh"
cat > "$WORK/bin/sudo" <<'EOF'
#!/bin/bash
set -euo pipefail
[ "$*" = 'apt-get purge --yes passt' ] || exit 99
case "$HAMN_TEST_PURGE" in
    remove) /bin/rm -f "$HAMN_TEST_PASST" ;;
    fail) exit 42 ;;
    keep) ;;
    *) exit 99 ;;
esac
EOF
chmod +x "$WORK/bin/sudo"
for scenario in absent present purge_failure retained_executable; do
    mode=remove
    expected=0
    if [ "$scenario" != absent ]; then
        printf '#!/bin/sh\nexit 0\n' > "$WORK/bin/passt"
        chmod +x "$WORK/bin/passt"
    fi
    case "$scenario" in
        purge_failure) mode=fail; expected=42 ;;
        retained_executable) mode=keep; expected=1 ;;
    esac
    actual=0
    PATH="$WORK/bin" HAMN_TEST_PURGE="$mode" HAMN_TEST_PASST="$WORK/bin/passt" \
        /bin/bash -euo pipefail "$WORK/network.sh" > "$WORK/log" 2>&1 || actual=$?
    if [ "$actual" -ne "$expected" ]; then
        cat "$WORK/log" >&2
        echo "FAIL: release network setup $scenario returned $actual, expected $expected" >&2
        exit 1
    fi
done
echo 'PASS: hosted networking removes passt and rejects incomplete package removal'
