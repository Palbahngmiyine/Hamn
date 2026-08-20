#!/bin/bash
set -euo pipefail

GUEST_ROOT=$(cd "$(dirname "$0")/.." && pwd)
PROJECT_ROOT=$(cd "$GUEST_ROOT/.." && pwd)
BUILDER=$GUEST_ROOT/image/build-ubuntu-24.04-arm64.sh
WORK=$(mktemp -d)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

bash -n "$BUILDER"
if "$BUILDER" >"$WORK/missing.out" 2>"$WORK/missing.err"; then
    echo "FAIL: guest image builder accepted missing required inputs" >&2
    exit 1
fi
grep -Fq 'HAMN_GUEST_BASE_IMAGE' "$WORK/missing.err"
grep -Fq 'docker.io,containerd,runc,containernetworking-plugins' "$BUILDER"
grep -Fq 'qemu-user-static,binfmt-support,dnsmasq' "$BUILDER"
grep -Fq '"components":["docker","buildkit","containerd","runc","cni","binfmt","dnsmasq","hamnd"]' "$BUILDER"
grep -Fq 'k3s-compatibility.json.sig' "$BUILDER"
grep -Fq 'make -C /opt/hamn/guest install' "$BUILDER"
grep -Fq 'groupadd --system hamn' "$BUILDER"
grep -Fq 'systemctl enable hamnd.service' "$BUILDER"
grep -Fq 'QEMU_BINFMT_INTERPRETER=/usr/bin/qemu-x86_64-static' \
    "$BUILDER"
grep -Fq '\x00\x00\x02\x00\x3e\x00' "$BUILDER"
grep -Fq 'update-binfmts --import qemu-x86_64' "$BUILDER"
grep -Fq 'systemctl enable binfmt-support.service' "$BUILDER"
if grep -Eq 'x86_64-binfmt-P|update-binfmts --enable qemu-x86_64' "$BUILDER"; then
    echo "FAIL: hosted image builder depends on a symlink or the build kernel binfmt state" >&2
    exit 1
fi
if grep -Eq 'shared|\.\./shared' "$BUILDER" "$GUEST_ROOT/Makefile"; then
    echo "FAIL: guest image source inputs retain the removed shared tree" >&2
    exit 1
fi

# Exercise the archive path in an isolated Git checkout. The injected
# untracked shared/ file must not become an immutable image input.
REPO=$WORK/repo
mkdir -p "$REPO"
git -C "$PROJECT_ROOT" archive --format=tar HEAD -- guest vendor |
    tar -C "$REPO" -xf -
cp "$BUILDER" "$REPO/guest/image/build-ubuntu-24.04-arm64.sh"
git -C "$REPO" init -q
git -C "$REPO" config user.name hamn-test
git -C "$REPO" config user.email hamn-test@example.invalid
git -C "$REPO" add guest vendor
git -C "$REPO" commit -qm 'guest image source fixture'
mkdir "$REPO/shared"
printf 'must not be archived\n' >"$REPO/shared/untracked-input"

mkdir "$WORK/bin"
cat >"$WORK/bin/sha256sum" <<'EOF'
#!/bin/bash
exec shasum -a 256 "$@"
EOF
cat >"$WORK/bin/ssh-keygen" <<'EOF'
#!/bin/bash
case "$1" in
    -lf) echo '256 SHA256:test hamn (ED25519)' ;;
    -Y) [ "${2:-}" = verify ] ;;
    *) exit 1 ;;
esac
EOF
cat >"$WORK/virt-customize" <<'EOF'
#!/bin/bash
set -euo pipefail
printf '%s\n' "$@" >"$HAMN_TEST_VIRT_ARGUMENTS"
for argument in "$@"; do
    case "$argument" in
        *:/tmp/hamn-guest-sources.tar.gz)
            tar -tzf "${argument%:/tmp/hamn-guest-sources.tar.gz}" \
                >"$HAMN_TEST_ARCHIVE_LIST"
            exit 0
            ;;
    esac
done
exit 1
EOF
cat >"$WORK/qemu-img" <<'EOF'
#!/bin/bash
set -euo pipefail
printf '%s\n' "$@" >>"$HAMN_TEST_QEMU_ARGUMENTS"
case "$1" in
    create)
        [ "$2" = -q ] && [ "$3" = -f ] && [ "$4" = qcow2 ]
        : >"$5"
        ;;
    convert)
        [ "$2" = -q ] && [ "$3" = -f ] && [ "$4" = qcow2 ]
        [ "$5" = -O ] && [ "$6" = qcow2 ]
        [ "$7" = -o ] && [ "$8" = compression_type=zlib ] && [ "$9" = -c ]
        cp "${10}" "${11}"
        ;;
    compare)
        [ "$2" = -q ] && [ "$3" = -f ] && [ "$4" = qcow2 ]
        [ "$5" = -F ] && [ "$6" = qcow2 ]
        cmp "$7" "$8"
        ;;
    *) exit 1 ;;
esac
EOF
cat >"$WORK/virt-resize" <<'EOF'
#!/bin/bash
set -euo pipefail
printf '%s\n' "$@" >"$HAMN_TEST_RESIZE_ARGUMENTS"
[ -f "${@: -2:1}" ] && [ -f "${@: -1}" ]
EOF
cat >"$WORK/guestfish" <<'EOF'
#!/bin/bash
set -euo pipefail
commands=$(cat)
case "$commands" in
    *"vfs-label /dev/sda3"*)
        printf '%s\n' "$@" >"$HAMN_TEST_GUESTFISH_LABEL_ARGUMENTS"
        printf '%s\n' "$commands" >"$HAMN_TEST_GUESTFISH_LABEL_COMMANDS"
        printf 'cloudimg-rootfs\n'
        ;;
    *"e2fsck-f /dev/sda3"*)
        printf '%s\n' "$@" >"$HAMN_TEST_GUESTFISH_RESIZE_ARGUMENTS"
        printf '%s\n' "$commands" >"$HAMN_TEST_GUESTFISH_RESIZE_COMMANDS"
        ;;
    *) exit 1 ;;
esac
EOF
chmod 0755 "$WORK/bin/sha256sum" "$WORK/bin/ssh-keygen" \
    "$WORK/virt-customize" "$WORK/qemu-img" "$WORK/virt-resize" \
    "$WORK/guestfish"

BASE=$WORK/base.img
MANIFEST=$WORK/k3s.json
SIGNATURE=$WORK/k3s.json.sig
PUBLIC_KEY=$WORK/release.pub
OUTPUT=$WORK/guest.img
ARCHIVE_LIST=$WORK/archive.list
VIRT_ARGUMENTS=$WORK/virt-arguments
QEMU_ARGUMENTS=$WORK/qemu-arguments
RESIZE_ARGUMENTS=$WORK/resize-arguments
GUESTFISH_LABEL_ARGUMENTS=$WORK/guestfish-label-arguments
GUESTFISH_LABEL_COMMANDS=$WORK/guestfish-label-commands
GUESTFISH_RESIZE_ARGUMENTS=$WORK/guestfish-resize-arguments
GUESTFISH_RESIZE_COMMANDS=$WORK/guestfish-resize-commands
printf 'base image fixture\n' >"$BASE"
printf '{}\n' >"$MANIFEST"
printf 'fixture signature\n' >"$SIGNATURE"
printf 'fixture public key\n' >"$PUBLIC_KEY"
BASE_SHA256=$(shasum -a 256 "$BASE" | awk '{print $1}')
PATH="$WORK/bin:$PATH" \
HAMN_GUEST_BASE_IMAGE="$BASE" \
HAMN_GUEST_BASE_SHA256="$BASE_SHA256" \
HAMN_GUEST_OUTPUT="$OUTPUT" \
HAMN_K3S_COMPATIBILITY_MANIFEST="$MANIFEST" \
HAMN_K3S_COMPATIBILITY_SIGNATURE="$SIGNATURE" \
HAMN_RELEASE_PUBLIC_KEY="$PUBLIC_KEY" \
HAMN_VIRT_CUSTOMIZE="$WORK/virt-customize" \
HAMN_QEMU_IMG="$WORK/qemu-img" \
HAMN_VIRT_RESIZE="$WORK/virt-resize" \
HAMN_GUESTFISH="$WORK/guestfish" \
HAMN_TEST_ARCHIVE_LIST="$ARCHIVE_LIST" \
HAMN_TEST_VIRT_ARGUMENTS="$VIRT_ARGUMENTS" \
HAMN_TEST_QEMU_ARGUMENTS="$QEMU_ARGUMENTS" \
HAMN_TEST_RESIZE_ARGUMENTS="$RESIZE_ARGUMENTS" \
HAMN_TEST_GUESTFISH_LABEL_ARGUMENTS="$GUESTFISH_LABEL_ARGUMENTS" \
HAMN_TEST_GUESTFISH_LABEL_COMMANDS="$GUESTFISH_LABEL_COMMANDS" \
HAMN_TEST_GUESTFISH_RESIZE_ARGUMENTS="$GUESTFISH_RESIZE_ARGUMENTS" \
HAMN_TEST_GUESTFISH_RESIZE_COMMANDS="$GUESTFISH_RESIZE_COMMANDS" \
"$REPO/guest/image/build-ubuntu-24.04-arm64.sh"

grep -Fxq 8G "$QEMU_ARGUMENTS"
grep -Fxq convert "$QEMU_ARGUMENTS"
grep -Fxq compression_type=zlib "$QEMU_ARGUMENTS"
grep -Fxq -- -c "$QEMU_ARGUMENTS"
grep -Fxq compare "$QEMU_ARGUMENTS"
grep -Fq 'MAX_RELEASE_ASSET_SIZE=2147483648' "$BUILDER"
grep -Fxq -- --format "$RESIZE_ARGUMENTS"
grep -Fxq -- --output-format "$RESIZE_ARGUMENTS"
grep -Fxq -- --expand "$RESIZE_ARGUMENTS"
grep -Fxq -- --no-expand-content "$RESIZE_ARGUMENTS"
grep -Fxq /dev/sda1 "$RESIZE_ARGUMENTS"
grep -Fxq -- --ro "$GUESTFISH_LABEL_ARGUMENTS"
grep -Fxq -- --format=qcow2 "$GUESTFISH_LABEL_ARGUMENTS"
grep -Fxq 'vfs-label /dev/sda3' "$GUESTFISH_LABEL_COMMANDS"
grep -Fxq -- --rw "$GUESTFISH_RESIZE_ARGUMENTS"
grep -Fxq -- --format=qcow2 "$GUESTFISH_RESIZE_ARGUMENTS"
grep -Fxq 'e2fsck-f /dev/sda3' "$GUESTFISH_RESIZE_COMMANDS"
grep -Fxq 'debug sh "resize2fs -f /dev/sda3"' "$GUESTFISH_RESIZE_COMMANDS"

FIXTURE_EPOCH=$(git -C "$REPO" show -s --format=%ct HEAD)
EXPECTED_CLOCK_COMMAND="date -u -s '@$FIXTURE_EPOCH'"
awk -v expected="$EXPECTED_CLOCK_COMMAND" '
    $0 == "--run-command" {
        getline
        if ($0 == expected && clock == 0) clock = NR
        next
    }
    $0 == "--install" { install = NR }
    END { exit !(clock > 0 && install > clock) }
' "$VIRT_ARGUMENTS" || {
    echo "FAIL: guest image builder did not set the source clock before package installation" >&2
    exit 1
}

grep -Fxq 'guest/Makefile' "$ARCHIVE_LIST"
grep -Fxq 'vendor/cjson/cJSON.c' "$ARCHIVE_LIST"
if grep -Eq '(^|/)shared(/|$)|untracked-input' "$ARCHIVE_LIST"; then
    echo "FAIL: untracked shared input was archived into the guest image" >&2
    exit 1
fi
grep -v '/$' "$ARCHIVE_LIST" | LC_ALL=C sort >"$WORK/archive-files"
git -C "$REPO" ls-tree -r --name-only HEAD -- guest vendor |
    LC_ALL=C sort >"$WORK/tracked-files"
diff -u "$WORK/tracked-files" "$WORK/archive-files"

echo "PASS: guest image builder uses only tracked immutable-image sources"
