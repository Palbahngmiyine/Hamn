#!/bin/bash
set -euo pipefail
# A failing assertion must name itself; CI otherwise shows only the exit.
trap 'echo "FAIL: ${BASH_SOURCE[0]}:$LINENO: $BASH_COMMAND" >&2' ERR

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
grep -Fq 'qemu-user-static,binfmt-support,dnsmasq-base,nftables' "$BUILDER"
grep -Fq '"components":["docker","buildkit","containerd","runc","cni","binfmt","dnsmasq","hamnd"]' "$BUILDER"
if grep -Eiq 'k3s|HAMN_RELEASE_PUBLIC_KEY' "$BUILDER" "$GUEST_ROOT/Makefile"; then
    echo "FAIL: new guest image still includes managed K3s inputs" >&2
    exit 1
fi
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
# Evidence and size gates run as the C tool built into the private workspace.
grep -Fq 'make -s --no-print-directory -C "$ROOT/guest" IMAGE_TOOL="$IMAGE_TOOL" image-tool' "$BUILDER"
for gate in 'evidence check' 'verify-raw' 'verify-size' 'evidence publish'; do
    grep -Fq "\"\$IMAGE_TOOL\" $gate" "$BUILDER"
done
# Neither the builder nor the image needs an interpreter: none is installed,
# pinned against autoremove, or required by the offline image check.
SLIM=$GUEST_ROOT/image/slim-guest.sh
if grep -Eiq 'python|\.py([^[:alnum:]_]|$)' "$BUILDER" "$SLIM"; then
    echo "FAIL: guest image build still installs or runs Python" >&2
    exit 1
fi
grep -Fq '/usr/local/libexec/hamn/guest-json' "$SLIM"
# Only linux/amd64 emulation and the six CNI plugins Hamn links stay; the
# image checks the filters took effect after installation.
EXCLUDES=$GUEST_ROOT/image/dpkg-excludes
for rule in \
    'path-exclude=/usr/bin/qemu-*-static' \
    'path-include=/usr/bin/qemu-x86_64-static' \
    'path-include=/usr/lib/binfmt.d/qemu-x86_64.conf' \
    'path-include=/usr/libexec/qemu-binfmt/x86_64-binfmt-P' \
    'path-exclude=/usr/lib/cni/*' \
    'path-include=/usr/lib/cni/bridge' 'path-include=/usr/lib/cni/host-local' \
    'path-include=/usr/lib/cni/loopback' 'path-include=/usr/lib/cni/portmap' \
    'path-include=/usr/lib/cni/firewall' 'path-include=/usr/lib/cni/tuning'; do
    grep -Fxq -- "$rule" "$EXCLUDES"
done
if grep -Eq '^path-exclude=.*(docker|containerd$|/runc$|dnsmasq|hamn)' "$EXCLUDES"; then
    echo "FAIL: dpkg path filters exclude a required runtime component" >&2
    exit 1
fi
grep -Fq 'for name in snapd lxd-installer ubuntu-cloud-minimal; do' "$SLIM"
grep -Fq 'for command in cloud-init sshd sudo netplan git; do' "$SLIM"
grep -Fq 'if installed dnsmasq; then' "$SLIM"

# Exercise the archive path in an isolated Git checkout. The injected
# untracked shared/ file must not become an immutable image input.
REPO=$WORK/repo
mkdir -p "$REPO"
# Through a file: a reading tar stops at the end-of-archive marker, and a
# writer still sending padding then dies of SIGPIPE (CI run 36148429857).
git -C "$PROJECT_ROOT" archive --format=tar -o "$WORK/sources.tar" HEAD -- \
    guest vendor host/image
tar -C "$REPO" -xf "$WORK/sources.tar"
cp "$GUEST_ROOT"/image/*.sh "$GUEST_ROOT"/image/*.c "$GUEST_ROOT"/image/*.h \
    "$GUEST_ROOT"/image/*.json "$GUEST_ROOT"/image/dpkg-excludes "$REPO/guest/image/"
mkdir -p "$REPO/guest/json"
cp "$GUEST_ROOT"/json/*.c "$GUEST_ROOT"/json/*.h "$REPO/guest/json/"
cp "$GUEST_ROOT/Makefile" "$REPO/guest/Makefile"
cp "$PROJECT_ROOT"/host/image/qcow2.{c,h} "$REPO/host/image/"
git -C "$REPO" init -q
git -C "$REPO" config user.name hamn-test
git -C "$REPO" config user.email hamn-test@example.invalid
git -C "$REPO" add guest vendor host
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
printf '%s\n' "$@" >>"$HAMN_TEST_VIRT_ARGUMENTS"
for argument in "$@"; do
    case "$argument" in
        *:/tmp/hamn-guest-sources.tar.gz)
            tar -tzf "${argument%:/tmp/hamn-guest-sources.tar.gz}" \
                >"$HAMN_TEST_ARCHIVE_LIST"
            exit 0
            ;;
    esac
done
exit 0
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
case " $* " in
    *" command "*) printf 'docker.io\t28.0\t12345\n'; exit 0 ;;
    *" fstrim "*)
        [[ " $* " == *" discard:enable "* ]]
        printf '%s\n' "$@" >"$HAMN_TEST_GUESTFISH_TRIM_ARGUMENTS"
        exit 0
        ;;
esac
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
OUTPUT=$WORK/guest.img
ARCHIVE_LIST=$WORK/archive.list
VIRT_ARGUMENTS=$WORK/virt-arguments
QEMU_ARGUMENTS=$WORK/qemu-arguments
RESIZE_ARGUMENTS=$WORK/resize-arguments
GUESTFISH_LABEL_ARGUMENTS=$WORK/guestfish-label-arguments
GUESTFISH_LABEL_COMMANDS=$WORK/guestfish-label-commands
GUESTFISH_RESIZE_ARGUMENTS=$WORK/guestfish-resize-arguments
GUESTFISH_RESIZE_COMMANDS=$WORK/guestfish-resize-commands
GUESTFISH_TRIM_ARGUMENTS=$WORK/guestfish-trim-arguments
printf 'base image fixture\n' >"$BASE"
BASE_SHA256=$(shasum -a 256 "$BASE" | awk '{print $1}')
if PATH="$WORK/bin:$PATH" \
HAMN_GUEST_BASE_IMAGE="$BASE" \
HAMN_GUEST_BASE_SHA256="$BASE_SHA256" \
HAMN_GUEST_OUTPUT="$OUTPUT" \
HAMN_GUEST_BASELINE_OUTPUT="$WORK/baseline.img" \
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
HAMN_TEST_GUESTFISH_TRIM_ARGUMENTS="$GUESTFISH_TRIM_ARGUMENTS" \
"$REPO/guest/image/build-ubuntu-24.04-arm64.sh" >"$WORK/build.out" 2>"$WORK/build.err"; then
    echo "FAIL: empty synthetic image passed actual extraction validation" >&2
    exit 1
fi
grep -Fq 'qcow2: file too small' "$WORK/build.err" || {
    echo "FAIL: the synthetic image was not rejected by qcow2 extraction" >&2
    cat "$WORK/build.err" >&2
    exit 1
}
[ ! -e "$OUTPUT" ]
[ ! -e "$WORK/baseline.img" ]
[ ! -e "$WORK/baseline.img.sha256" ]

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
# The journal is recreated and checked before fstrim, in the builder and in
# generated variations alike.
JOURNAL_RESET='tune2fs -O ^has_journal /dev/sda3 && tune2fs -j /dev/sda3'
grep -Fq -- "#define JOURNAL_RESET \"$JOURNAL_RESET\"" "$GUEST_ROOT/image/variations.c"
awk -v reset="$JOURNAL_RESET" '
    $0 == reset && previous == "sh" { journal = NR }
    $0 == "e2fsck-f" { check = NR }
    $0 == "mount" { mount = NR }
    $0 == "fstrim" { trim = NR }
    { previous = $0 }
    END { exit !(journal > 0 && check > journal && mount > check && trim > mount) }
' "$GUESTFISH_TRIM_ARGUMENTS" || {
    echo "FAIL: the builder must recreate and check the journal before fstrim" >&2
    exit 1
}

FIXTURE_EPOCH=$(git -C "$REPO" show -s --format=%ct HEAD)
EXPECTED_CLOCK_COMMAND="date -u -s '@$FIXTURE_EPOCH'"
awk -v expected="$EXPECTED_CLOCK_COMMAND" '
    $0 == "--run-command" {
        getline
        if ($0 == expected && clock == 0) clock = NR
        if ($0 == "timeout 30 getent ahostsv4 ports.ubuntu.com") dns = NR
        next
    }
    $0 == "--upload" {
        getline
        if ($0 ~ /\/guest\/image\/dpkg-excludes:\/etc\/dpkg\/dpkg.cfg.d\/hamn-excludes$/) excludes = NR
        next
    }
    $0 == "--install" { install = NR }
    END { exit !(clock > 0 && dns > clock && excludes > dns && install > excludes) }
' "$VIRT_ARGUMENTS" || {
    echo "FAIL: guest image builder must set its clock, check DNS and install its dpkg path filters before package installation" >&2
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

echo "PASS: tracked immutable sources, configured compaction, and invalid image publication rejection"
