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
chmod 0755 "$WORK/bin/sha256sum" "$WORK/bin/ssh-keygen" "$WORK/virt-customize"

BASE=$WORK/base.img
MANIFEST=$WORK/k3s.json
SIGNATURE=$WORK/k3s.json.sig
PUBLIC_KEY=$WORK/release.pub
OUTPUT=$WORK/guest.img
ARCHIVE_LIST=$WORK/archive.list
VIRT_ARGUMENTS=$WORK/virt-arguments
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
HAMN_TEST_ARCHIVE_LIST="$ARCHIVE_LIST" \
HAMN_TEST_VIRT_ARGUMENTS="$VIRT_ARGUMENTS" \
"$REPO/guest/image/build-ubuntu-24.04-arm64.sh"

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
