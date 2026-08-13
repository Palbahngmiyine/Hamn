#!/bin/bash
# Exercise exact Hamn candidate bytes on a physical Apple Silicon validator.
# Hamn is isolated below a temporary HOME. Colima coexistence is inspected
# read-only.
set -euo pipefail
export LC_ALL=C
umask 077

WORK=
TEST_HOME=
HAMN=
VALIDATOR_PATH=
VALIDATOR_HOME=
COLIMA_ROOT=
COLIMA_BEFORE_HASH=
COLIMA_BINARY_BEFORE_HASH=
COLIMA_INSTANCES_BEFORE_HASH=
DOCKER=
KUBECTL=
COLIMA=
PYTHON3=
HOST_ARTIFACT=
HOST_ARTIFACT_HASH=
GUEST_ARTIFACT=
GUEST_ARTIFACT_HASH=
CANDIDATE_VERSION=
ARTIFACT_ROOT=
COMPOSE_DIR=

usage() {
    cat <<'EOF'
usage: physical-e2e.sh [--preflight]

Required environment supplied by release-gate:
  HAMN_CANDIDATE_DIR   exact candidate artifact directory
  HAMN_CANDIDATE_JSON  candidate metadata
  HAMN_E2E_OUTPUT      empty absolute JSON output path

The physical validator needs Docker CLI with Compose and buildx, kubectl, curl,
ssh-keygen, Python 3, and an existing Colima installation. It never starts,
stops, configures, installs, updates, or deletes Colima; it checks coexistence
only.

--preflight checks the physical validator tools and state without installing or
starting Hamn. The Docker executable must be an external Docker CLI, never
Hamn itself or a legacy docker -> hamn shim.
EOF
}

fail() {
    echo "hamn physical E2E: $*" >&2
    exit 1
}

sha256_file() {
    shasum -a 256 "$1" | awk '{print $1}'
}

tool_path() {
    local path
    path=$(command -v "$1" 2>/dev/null || true)
    [ -n "$path" ] && [ "${path#/}" != "$path" ] && [ -x "$path" ] ||
        return 1
    printf '%s\n' "$path"
}

require_tool() {
    local path
    path=$(tool_path "$1" || true)
    [ -n "$path" ] || fail "required validator tool is unavailable: $1"
    printf '%s\n' "$path"
}

require_value() {
    local variable=$1 value
    value=$(printenv "$variable" || true)
    [ -n "$value" ] || fail "$variable is required"
}

canonical_regular_path() {
    "$PYTHON3" - "$1" <<'PY'
import os
import stat
import sys

path = os.path.realpath(sys.argv[1])
if not os.path.isabs(path):
    raise SystemExit("resolved executable path is not absolute")
if not stat.S_ISREG(os.stat(path).st_mode):
    raise SystemExit("resolved executable path is not a regular file")
print(path)
PY
}

same_executable() {
    "$PYTHON3" - "$1" "$2" <<'PY'
import os
import sys

try:
    same = os.path.samefile(sys.argv[1], sys.argv[2])
except OSError as error:
    raise SystemExit(str(error))
print("yes" if same else "no")
PY
}

require_external_docker_cli() {
    local docker_real hamn_path same

    docker_real=$(canonical_regular_path "$DOCKER") ||
        fail "Docker executable cannot be resolved safely: $DOCKER"
    if [ "${docker_real##*/}" = hamn ]; then
        fail "required external Docker CLI resolves to Hamn or a legacy docker -> hamn shim: $DOCKER"
    fi

    hamn_path=$(tool_path hamn || true)
    if [ -n "$hamn_path" ]; then
        same=$(same_executable "$DOCKER" "$hamn_path") ||
            fail "Docker and Hamn executable identity cannot be compared safely"
        case "$same" in
        yes)
            fail "required external Docker CLI is Hamn or a legacy docker -> hamn shim: $DOCKER"
            ;;
        no)
            ;;
        *)
            fail "Docker and Hamn executable identity comparison was malformed"
            ;;
        esac
    fi

    "$DOCKER" context --help >/dev/null 2>&1 ||
        fail "required external Docker CLI does not support docker context: $DOCKER"
}

require_validator_environment() {
    [ -n "${HOME:-}" ] && [ "${HOME#/}" != "$HOME" ] ||
        fail "validator HOME must be an absolute path"
    VALIDATOR_HOME=$HOME
    VALIDATOR_PATH="/opt/homebrew/bin:/usr/local/bin:$VALIDATOR_HOME/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
    export PATH=$VALIDATOR_PATH
    [ "$(uname -m)" = arm64 ] || fail "validator must be Apple Silicon"
    PYTHON3=$(require_tool python3)
    DOCKER=$(require_tool docker)
    require_external_docker_cli
    KUBECTL=$(require_tool kubectl)
    COLIMA=$(require_tool colima)
    require_tool curl >/dev/null
    require_tool ssh-keygen >/dev/null
    require_tool tar >/dev/null
    require_tool go >/dev/null
    require_tool node >/dev/null
    require_tool npm >/dev/null
    require_tool java >/dev/null
    require_tool mvn >/dev/null
    "$DOCKER" compose version >/dev/null ||
        fail "Docker Compose v2 is unavailable"
    "$DOCKER" buildx version >/dev/null ||
        fail "Docker buildx is unavailable"
    "$COLIMA" version >/dev/null ||
        fail "Colima is unavailable"
    COLIMA_ROOT=$VALIDATOR_HOME/.colima
    [ -d "$COLIMA_ROOT" ] && [ ! -L "$COLIMA_ROOT" ] ||
        fail "an existing non-symlink Colima state directory is required: $COLIMA_ROOT"
}

require_candidate_environment() {
    require_value HAMN_CANDIDATE_DIR
    require_value HAMN_CANDIDATE_JSON
    require_value HAMN_E2E_OUTPUT
    [ -d "$HAMN_CANDIDATE_DIR" ] && [ ! -L "$HAMN_CANDIDATE_DIR" ] ||
        fail "candidate directory is unsafe"
    [ -f "$HAMN_CANDIDATE_JSON" ] && [ ! -L "$HAMN_CANDIDATE_JSON" ] ||
        fail "candidate metadata is unsafe"
    case "$HAMN_E2E_OUTPUT" in
    /*) ;;
    *) fail "E2E output must be an absolute path" ;;
    esac
    [ ! -e "$HAMN_E2E_OUTPUT" ] || fail "E2E output already exists"
}

require_environment() {
    require_validator_environment
    require_candidate_environment
}

read_candidate() {
    HOST_ARTIFACT=
    HOST_ARTIFACT_HASH=
    GUEST_ARTIFACT=
    GUEST_ARTIFACT_HASH=
    while IFS=$'\t' read -r key value; do
        case "$key" in
        host) HOST_ARTIFACT=$value ;;
        hostHash) HOST_ARTIFACT_HASH=$value ;;
        guest) GUEST_ARTIFACT=$value ;;
        guestHash) GUEST_ARTIFACT_HASH=$value ;;
        version) CANDIDATE_VERSION=$value ;;
        *) fail "candidate metadata emitted an unknown field" ;;
        esac
    done < <(python3 - "$HAMN_CANDIDATE_JSON" "$HAMN_CANDIDATE_DIR" <<'PY'
import json
import os
import re
import sys

metadata, directory = sys.argv[1:]
with open(metadata, encoding="utf-8") as source:
    candidate = json.load(source)
if candidate.get("kind") != "hamn-release-candidate":
    raise SystemExit("invalid candidate identity")
version = candidate.get("version")
if not isinstance(version, str) or not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", version):
    raise SystemExit("invalid candidate version")
values = {}
for item in candidate.get("artifacts", []):
    if not isinstance(item, dict) or set(item) != {"name", "sha256"}:
        raise SystemExit("invalid candidate artifact")
    name = item["name"]
    digest = item["sha256"]
    if not isinstance(name, str) or "/" in name or "\t" in name or not re.fullmatch(
            r"[A-Za-z0-9._-]+", name):
        raise SystemExit("unsafe candidate artifact name")
    if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise SystemExit("invalid candidate artifact hash")
    values[name] = digest
host = [name for name in values if name.endswith("-darwin-arm64.tar.gz")]
guest = [name for name in values if name.endswith("-ubuntu-24.04-arm64.img")]
if len(host) != 1 or len(guest) != 1:
    raise SystemExit("candidate host or guest artifact is missing")
for label, name in (("host", host[0]), ("guest", guest[0])):
    path = os.path.join(directory, name)
    if not os.path.isfile(path) or os.path.islink(path):
        raise SystemExit("candidate artifact is unsafe: " + name)
    print(label + "\t" + path)
    print(label + "Hash\t" + values[name])
print("version\t" + version)
PY
)
    [ -n "$HOST_ARTIFACT" ] && [ -n "$HOST_ARTIFACT_HASH" ] &&
        [ -n "$GUEST_ARTIFACT" ] && [ -n "$GUEST_ARTIFACT_HASH" ] &&
        [ -n "$CANDIDATE_VERSION" ] ||
        fail "candidate metadata fields are incomplete"
    [ "$(sha256_file "$HOST_ARTIFACT")" = "$HOST_ARTIFACT_HASH" ] ||
        fail "candidate host artifact hash changed"
    [ "$(sha256_file "$GUEST_ARTIFACT")" = "$GUEST_ARTIFACT_HASH" ] ||
        fail "candidate guest artifact hash changed"
}

colima_state_hash() {
    "$PYTHON3" - "$COLIMA_ROOT" "$VALIDATOR_HOME/.docker" <<'PY'
import hashlib
import os
import stat
import sys

roots = (("colima", sys.argv[1]), ("docker", sys.argv[2]))
digest = hashlib.sha256()

def add(text):
    digest.update(text.encode("utf-8", "surrogateescape"))
    digest.update(b"\0")

def visit(label, path):
    try:
        info = os.lstat(path)
    except FileNotFoundError:
        add(label + ":absent")
        return
    add(label + ":type=" + str(stat.S_IFMT(info.st_mode)))
    add(label + ":mode=" + str(stat.S_IMODE(info.st_mode)))
    add(label + ":uid=" + str(info.st_uid))
    add(label + ":gid=" + str(info.st_gid))
    if stat.S_ISREG(info.st_mode):
        add(label + ":size=" + str(info.st_size))
        content = hashlib.sha256()
        with open(path, "rb", buffering=0) as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                content.update(chunk)
        add(label + ":content=" + content.hexdigest())
        return
    if stat.S_ISLNK(info.st_mode):
        add(label + ":target=" + os.readlink(path))
        return
    if stat.S_ISDIR(info.st_mode):
        with os.scandir(path) as entries:
            names = sorted(entry.name for entry in entries)
        for name in names:
            visit(label + "/" + name, os.path.join(path, name))

for label, path in roots:
    visit(label, path)
print(digest.hexdigest())
PY
}

colima_binary_hash() {
    "$PYTHON3" - "$COLIMA" <<'PY'
import hashlib
import os
import stat
import sys

path = os.path.abspath(sys.argv[1])
resolved = os.path.realpath(path)
if not os.path.isabs(resolved):
    raise SystemExit("resolved Colima path is not absolute")

digest = hashlib.sha256()

def add(value):
    digest.update(value.encode("utf-8", "surrogateescape"))
    digest.update(b"\0")

def add_lstat(label, value):
    info = os.lstat(value)
    add(label + ":path=" + value)
    add(label + ":type=" + str(stat.S_IFMT(info.st_mode)))
    add(label + ":mode=" + str(stat.S_IMODE(info.st_mode)))
    add(label + ":uid=" + str(info.st_uid))
    add(label + ":gid=" + str(info.st_gid))
    add(label + ":device=" + str(info.st_dev))
    add(label + ":inode=" + str(info.st_ino))
    add(label + ":size=" + str(info.st_size))
    if stat.S_ISLNK(info.st_mode):
        add(label + ":target=" + os.readlink(value))
    return info

source = add_lstat("command", path)
target = add_lstat("resolved", resolved)
if not stat.S_ISREG(target.st_mode):
    raise SystemExit("resolved Colima executable is not a regular file")
content = hashlib.sha256()
with open(resolved, "rb", buffering=0) as binary:
    for chunk in iter(lambda: binary.read(1024 * 1024), b""):
        content.update(chunk)
add("resolved:content=" + content.hexdigest())
print(digest.hexdigest())
PY
}

# `colima list --json` is a read-only inventory query.  The state-directory
# digest below catches profile configuration, contexts, and sockets; this
# inventory separately proves that the validator had at least one actual
# Colima profile/VM to preserve rather than an empty ~/.colima directory.
colima_instance_inventory_hash() {
    "$COLIMA" list --json | "$PYTHON3" -c '
import hashlib
import json
import re
import sys

raw = sys.stdin.buffer.read(1024 * 1024 + 1)
if len(raw) > 1024 * 1024:
    raise SystemExit("Colima instance inventory is too large")
try:
    text = raw.decode("utf-8")
except UnicodeDecodeError as error:
    raise SystemExit("Colima instance inventory is not UTF-8: " + str(error))

def reject_constant(value):
    raise ValueError("invalid JSON constant: " + value)

try:
    stripped = text.strip()
    if not stripped:
        raise ValueError("no Colima profiles or VMs were reported")
    if stripped.startswith("["):
        records = json.loads(stripped, parse_constant=reject_constant)
    else:
        records = [json.loads(line, parse_constant=reject_constant)
                   for line in text.splitlines() if line.strip()]
except (ValueError, json.JSONDecodeError) as error:
    raise SystemExit("invalid Colima instance inventory: " + str(error))

if not isinstance(records, list) or not records:
    raise SystemExit("no Colima profiles or VMs were reported")
canonical = []
for record in records:
    if not isinstance(record, dict):
        raise SystemExit("invalid Colima instance record")
    name = record.get("name")
    status = record.get("status")
    if (not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", name) or
            not isinstance(status, str) or not status):
        raise SystemExit("invalid Colima profile or VM record")
    canonical.append(json.dumps(record, ensure_ascii=True, sort_keys=True,
                                separators=(",", ":"), allow_nan=False))
canonical.sort()
digest = hashlib.sha256()
for record in canonical:
    digest.update(record.encode("ascii"))
    digest.update(b"\\0")
print(digest.hexdigest())
'
}

prepare_workspace() {
    WORK=$(mktemp -d "${TMPDIR:-/tmp}/hamn-physical-e2e.XXXXXX") ||
        fail "cannot create E2E workspace"
    [ -d "$WORK" ] && [ ! -L "$WORK" ] ||
        fail "E2E workspace is unsafe"
    chmod 0700 "$WORK"
    TEST_HOME=$WORK/home
    mkdir -m 0700 "$TEST_HOME"
}

extract_candidate() {
    ARTIFACT_ROOT=$("$PYTHON3" - "$HOST_ARTIFACT" "$WORK/extract" \
        "$CANDIDATE_VERSION" <<'PY'
import os
import posixpath
import sys
import tarfile

archive, destination, version = sys.argv[1:]
with tarfile.open(archive, "r:gz") as bundle:
    members = bundle.getmembers()
    if not members:
        raise SystemExit("empty host artifact")
    roots = set()
    actual = set()
    for member in members:
        name = member.name
        if name.startswith("/") or "\\" in name:
            raise SystemExit("unsafe host artifact path")
        normalized = posixpath.normpath(name)
        if normalized in (".", "..") or normalized.startswith("../") or \
                normalized != name.rstrip("/"):
            raise SystemExit("unsafe host artifact path")
        if not (member.isdir() or member.isreg()):
            raise SystemExit("host artifact contains a non-regular entry")
        roots.add(normalized.split("/", 1)[0])
        actual.add(name.rstrip("/"))
    expected_root = "hamn-" + version + "-darwin-arm64"
    if roots != {expected_root}:
        raise SystemExit("host artifact root does not match candidate version")
    required = {
        expected_root + "/bin/hamn",
        expected_root + "/scripts/install-host.sh",
        expected_root + "/scripts/update-host.sh",
        expected_root + "/packaging/release/physical-e2e.sh",
    }
    missing = required - actual
    if missing:
        raise SystemExit("host artifact is missing: " + ", ".join(sorted(missing)))
    os.makedirs(destination, mode=0o700, exist_ok=True)
    for member in members:
        bundle.extract(member, destination)
print(os.path.join(destination, expected_root))
PY
) || fail "cannot validate or extract candidate host artifact"
    [ -d "$ARTIFACT_ROOT" ] && [ ! -L "$ARTIFACT_ROOT" ] &&
        [ -f "$ARTIFACT_ROOT/bin/hamn" ] && [ ! -L "$ARTIFACT_ROOT/bin/hamn" ] &&
        [ -x "$ARTIFACT_ROOT/bin/hamn" ] &&
        [ -f "$ARTIFACT_ROOT/scripts/install-host.sh" ] &&
        [ ! -L "$ARTIFACT_ROOT/scripts/install-host.sh" ] ||
        fail "extracted candidate host artifact is incomplete"
}

stage_candidate_guest() {
    local cache guest marker selection
    cache=$TEST_HOME/.hamn/cache
    mkdir -p "$cache"
    chmod 0755 "$TEST_HOME/.hamn" "$cache"
    guest=$cache/hamn-guest-$GUEST_ARTIFACT_HASH.img
    marker=$guest.verified
    selection=$cache/guest-image.json
    cp "$GUEST_ARTIFACT" "$guest"
    chmod 0644 "$guest"
    [ "$(sha256_file "$guest")" = "$GUEST_ARTIFACT_HASH" ] ||
        fail "staged guest image differs from the candidate"
    printf '%s\n' "$GUEST_ARTIFACT_HASH" >"$marker"
    chmod 0644 "$marker"
    printf '{"schemaVersion":1,"file":"%s","sha256":"%s"}\n' \
        "$(basename "$guest")" "$GUEST_ARTIFACT_HASH" >"$selection"
    chmod 0644 "$selection"
}

install_candidate() {
    local test_home_real generation_root link_target

    mkdir -p "$TEST_HOME/.local/bin" "$TEST_HOME/.local/share"
    chmod 0700 "$TEST_HOME/.local" "$TEST_HOME/.local/share"
    # update-host.sh requires its managed command directory to be a private
    # user-owned 0755 directory. The enclosing E2E HOME remains 0700.
    chmod 0755 "$TEST_HOME/.local/bin"
    env -i HOME="$TEST_HOME" PATH="$VALIDATOR_PATH" \
        /bin/bash "$ARTIFACT_ROOT/scripts/install-host.sh" \
        "$ARTIFACT_ROOT/bin/hamn" "$TEST_HOME/.local/bin" \
        "$TEST_HOME/.local/share/hamn/src" >/dev/null
    HAMN=$TEST_HOME/.local/bin/hamn
    test_home_real=$(cd "$TEST_HOME" && pwd -P) ||
        fail "candidate installer test home cannot be resolved safely"
    generation_root=$test_home_real/.local/share/hamn/src/.hamn-generations
    [ -L "$HAMN" ] && [ -d "$generation_root" ] &&
        [ ! -L "$generation_root" ] ||
        fail "candidate installer did not produce a managed Hamn binary"
    link_target=$(readlink "$HAMN") ||
        fail "candidate installer produced an unreadable Hamn command link"
    case "$link_target" in
    "$generation_root"/*)
        ;;
    *)
        fail "candidate installer Hamn command link is outside its managed generation root"
        ;;
    esac
    if ! "$PYTHON3" - "$link_target" "$generation_root" <<'PY'
import os
import re
import stat
import sys

target, root = sys.argv[1:]
if not target.startswith(root + "/"):
    raise SystemExit(1)
relative = target[len(root) + 1:]
if not re.fullmatch(r"[0-9a-f]{64}-[A-Za-z0-9]{6}/bin/hamn", relative):
    raise SystemExit(1)
try:
    info = os.lstat(target)
except OSError:
    raise SystemExit(1)
if not stat.S_ISREG(info.st_mode) or stat.S_ISLNK(info.st_mode) or \
        not os.access(target, os.X_OK):
    raise SystemExit(1)
PY
    then
        fail "candidate installer Hamn command link has an invalid managed generation identity"
    fi
    "$HAMN" version | grep -Fxq "hamn ${CANDIDATE_VERSION#v}" ||
        fail "installed candidate binary version is incorrect"
}

run_hamn() {
    env -i HOME="$TEST_HOME" PATH="$VALIDATOR_PATH" "$HAMN" "$@"
}

run_docker() {
    env -i HOME="$TEST_HOME" PATH="$VALIDATOR_PATH" \
        DOCKER_CONFIG="$TEST_HOME/.docker" "$DOCKER" "$@"
}

run_compose() {
    env -i HOME="$TEST_HOME" PATH="$VALIDATOR_PATH" \
        DOCKER_CONFIG="$TEST_HOME/.docker" \
        COMPOSE_PROJECT_NAME=hamn-e2e-compose \
        COMPOSE_BIND_SOURCE="$COMPOSE_DIR/bind" \
        COMPOSE_APP_ENV=compose-e2e \
        "$DOCKER" --context hamn compose --project-directory "$COMPOSE_DIR" \
        -f "$COMPOSE_DIR/compose.yaml" "$@"
}

run_sdk() {
    env -i HOME="$TEST_HOME" PATH="$VALIDATOR_PATH" \
        DOCKER_HOST="unix://$TEST_HOME/.hamn/default/docker.sock" \
        TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE=/var/run/docker.sock \
        TESTCONTAINERS_HOST_OVERRIDE=host.docker.internal "$@"
}

mark_test() {
    local name=$1
    case "$name" in
    lifecycle|multiProfile|staleSocket|dockerContextRestore|mount|network|tcpPort|udpPort|amd64|rosetta|dockerCli|compose|buildx|dockerSdkGo|dockerSdkPython|testcontainersJava|testcontainersGo|testcontainersPython|testcontainersNode|k3s|updateRollback|softDeleteRecovery|hardDelete|uninstall|colimaCoexistence)
        ;;
    *) fail "unknown E2E test name: $name" ;;
    esac
    [ -n "$WORK" ] || fail "E2E workspace is unavailable"
    if grep -Fxq "$name" "$WORK/tests.txt" 2>/dev/null; then
        fail "E2E test was recorded twice: $name"
    fi
    printf '%s\n' "$name" >>"$WORK/tests.txt"
}

assert_running_status() {
    local path=$WORK/status.json
    run_hamn status --json >"$path"
    "$PYTHON3" - "$path" "$TEST_HOME/.hamn/default/docker.sock" <<'PY'
import json
import sys

path, socket = sys.argv[1:]
with open(path, encoding="utf-8") as source:
    value = json.load(source)
if set(value) != {
        "schemaVersion", "profile", "state", "cpus", "memoryMiB", "diskGiB",
        "dockerContext", "docker", "cri", "kubernetes", "directory", "ip"}:
    raise SystemExit("status schema is incomplete")
if value["schemaVersion"] != 2 or value["profile"] != "default" or \
        value["state"] != "running" or value["dockerContext"] != "hamn":
    raise SystemExit("default runtime status is incorrect")
docker = value["docker"]
cri = value["cri"]
kubernetes = value["kubernetes"]
if docker != {"socket": socket, "apiReady": True}:
    raise SystemExit("Docker API readiness or socket is incorrect")
if cri != {"ready": True, "namespace": "k8s.io"}:
    raise SystemExit("CRI readiness is incorrect")
if kubernetes != {"enabled": False, "ready": False}:
    raise SystemExit("Kubernetes readiness is incorrect before activation")
if not isinstance(value["ip"], str) or not value["ip"]:
    raise SystemExit("guest IP is unavailable")
PY
}

cleanup() {
    local rc=$?
    trap - EXIT
    set +e
    if [ -n "$HAMN" ] && [ -x "$HAMN" ]; then
        run_docker --context hamn rm -f hamn-e2e-tcp hamn-e2e-udp \
            >/dev/null 2>&1
        run_docker --context hamn rm -f hamn-e2e-moby >/dev/null 2>&1
        run_docker --context hamn buildx rm -f hamn-e2e-builder \
            >/dev/null 2>&1
        run_hamn stop -p default >/dev/null 2>&1
        run_hamn stop -p second >/dev/null 2>&1
        run_hamn stop -p recovery >/dev/null 2>&1
        printf 'y\n' | run_hamn uninstall >/dev/null 2>&1
    fi
    if [ -n "$WORK" ] && [ -d "$WORK" ] && [ ! -L "$WORK" ]; then
        /bin/rm -rf -- "$WORK"
    fi
    exit "$rc"
}

exercise_lifecycle_and_core_docker() {
    local initial_context stale_socket mount_file monitor_source monitor_binary
    initial_context=$(run_docker context show)
    [ -n "$initial_context" ] || fail "Docker CLI has no active context"

    # The default HOME virtiofs share is writable.  Enable the experimental
    # bridge before boot so the following monitor proves real guest inotify
    # attributes/close-write events, not merely that virtiofs reads observe a
    # host write. The bridge never rewrites file contents.
    run_hamn configure --mount-inotify true
    run_hamn start --template=false
    assert_running_status
    [ "$(run_docker context show)" = hamn ] ||
        fail "Hamn did not activate its default Docker context"
    run_docker --context hamn version --format '{{.Server.Version}}' \
        >/dev/null
    run_docker --context hamn run --rm alpine:3.21 /bin/true
    mark_test dockerCli

    stale_socket=$TEST_HOME/.hamn/default/docker.sock
    [ -S "$stale_socket" ] && [ "$(stat -f '%Lp' "$stale_socket")" = 600 ] ||
        fail "profile Docker socket is not a private Unix socket"
    run_hamn stop
    [ "$(run_docker context show)" = "$initial_context" ] ||
        fail "Hamn did not restore the prior Docker context"
    mark_test dockerContextRestore

    "$PYTHON3" - "$stale_socket" <<'PY'
import os
import socket
import sys

path = sys.argv[1]
if os.path.lexists(path):
    os.unlink(path)
sock = socket.socket(socket.AF_UNIX)
sock.bind(path)
sock.close()
PY
    [ -S "$stale_socket" ] || fail "cannot create a stale test socket"
    run_hamn start --template=false
    assert_running_status
    [ -S "$stale_socket" ] && [ "$(stat -f '%Lp' "$stale_socket")" = 600 ] ||
        fail "Hamn did not replace the stale Docker socket"
    mark_test staleSocket

    mount_file=$TEST_HOME/hamn-mount-check.txt
    printf 'first-host-write\n' >"$mount_file"
    run_hamn ssh -- /bin/cat "$mount_file" >"$WORK/mount-first.txt"
    grep -Fxq 'first-host-write' "$WORK/mount-first.txt" ||
        fail "guest cannot read the default writable HOME mount"
    printf 'second-host-write\n' >"$mount_file"
    run_hamn ssh -- /bin/cat "$mount_file" >"$WORK/mount-second.txt"
    grep -Fxq 'second-host-write' "$WORK/mount-second.txt" ||
        fail "ordinary host writes do not reach the guest HOME mount"

    monitor_source=$TEST_HOME/hamn-mount-inotify-monitor.c
    monitor_binary=$TEST_HOME/hamn-mount-inotify-monitor
    cat >"$monitor_source" <<'EOF'
#include <poll.h>
#include <stdio.h>
#include <sys/inotify.h>
#include <time.h>
#include <unistd.h>

int main(int argc, char **argv)
{
    if (argc != 2)
        return 2;
    int fd = inotify_init1(IN_CLOEXEC);
    if (fd < 0)
        return 3;
    if (inotify_add_watch(fd, argv[1], IN_ATTRIB | IN_CLOSE_WRITE) < 0) {
        close(fd);
        return 4;
    }
    fputs("ready\\n", stdout);
    if (fflush(stdout) != 0) {
        close(fd);
        return 5;
    }
    struct timespec start, now;
    if (clock_gettime(CLOCK_MONOTONIC, &start) != 0) {
        close(fd);
        return 6;
    }
    unsigned seen = 0;
    while (seen != (IN_ATTRIB | IN_CLOSE_WRITE)) {
        if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
            close(fd);
            return 7;
        }
        long long elapsed = (long long)(now.tv_sec - start.tv_sec) * 1000 +
            (now.tv_nsec - start.tv_nsec) / 1000000;
        if (elapsed < 0 || elapsed >= 10000) {
            close(fd);
            return 8;
        }
        struct pollfd wait = { .fd = fd, .events = POLLIN };
        int ready = poll(&wait, 1, (int)(10000 - elapsed));
        if (ready != 1 || !(wait.revents & POLLIN)) {
            close(fd);
            return 9;
        }
        char bytes[4096];
        ssize_t received = read(fd, bytes, sizeof(bytes));
        if (received < 0) {
            close(fd);
            return 10;
        }
        for (ssize_t offset = 0;
             offset + (ssize_t)sizeof(struct inotify_event) <= received;) {
            const struct inotify_event *event =
                (const struct inotify_event *)(const void *)(bytes + offset);
            ssize_t length = (ssize_t)sizeof(*event) + event->len;
            if (length < (ssize_t)sizeof(*event) || offset + length > received) {
                close(fd);
                return 11;
            }
            seen |= event->mask & (IN_ATTRIB | IN_CLOSE_WRITE);
            offset += length;
        }
    }
    close(fd);
    fputs("changed\\n", stdout);
    return fflush(stdout) == 0 ? 0 : 12;
}
EOF
    run_hamn ssh -- /usr/bin/gcc -O2 "$monitor_source" -o "$monitor_binary"
    if ! run_hamn ssh -- "$monitor_binary" "$mount_file" | (
        IFS= read -r ready
        [ "$ready" = ready ] || exit 1
        printf 'third-host-write\\n' >"$mount_file"
        IFS= read -r result
        [ "$result" = changed ] || exit 1
        ! IFS= read -r extra
    ); then
        fail "mountInotify did not produce guest IN_ATTRIB and IN_CLOSE_WRITE events"
    fi
    mark_test mount

    run_docker --context hamn run --rm alpine:3.21 sh -ec \
        'getent hosts host.docker.internal >/dev/null || nslookup host.docker.internal >/dev/null'
    mark_test network
    mark_test lifecycle
}

exercise_multi_profile_and_delete() {
    run_hamn start --profile second --template=false
    run_docker --context hamn-second version --format '{{.Server.Version}}' \
        >/dev/null
    run_docker --context hamn-second run --rm alpine:3.21 /bin/true
    run_hamn stop --profile second
    [ "$(run_docker context show)" = hamn ] ||
        fail "stopping a secondary profile did not restore the default Hamn context"
    mark_test multiProfile

    printf 'y\n' | run_hamn delete --profile second --data
    [ ! -e "$TEST_HOME/.hamn/second" ] &&
        [ ! -L "$TEST_HOME/.hamn/second" ] ||
        fail "hard profile delete left secondary profile data behind"
    mark_test hardDelete
}

exercise_soft_delete_recovery() {
    local profile=recovery volume=hamn-e2e-recovery-data
    run_hamn start --profile "$profile" --template=false
    run_docker --context "hamn-$profile" volume create "$volume" >/dev/null
    run_docker --context "hamn-$profile" run --rm \
        -v "$volume:/state" alpine:3.21 sh -ec \
        'printf "retained-after-soft-delete\\n" >/state/marker'

    run_hamn delete --profile "$profile"
    [ -f "$TEST_HOME/.hamn/$profile/disk.img" ] &&
        [ -f "$TEST_HOME/.hamn/$profile/deleted" ] ||
        fail "soft delete did not preserve the profile disk and deletion marker"
    [ "$(run_docker context show)" = hamn ] ||
        fail "soft deleting a secondary profile did not restore the default Hamn context"

    run_hamn start --profile "$profile" --template=false
    run_docker --context "hamn-$profile" run --rm \
        -v "$volume:/state" alpine:3.21 /bin/cat /state/marker |
        grep -Fxq retained-after-soft-delete ||
        fail "Docker volume data did not survive same-profile soft-delete recovery"
    run_hamn stop --profile "$profile"
    printf 'y\n' | run_hamn delete --profile "$profile" --data
    [ ! -e "$TEST_HOME/.hamn/$profile" ] &&
        [ ! -L "$TEST_HOME/.hamn/$profile" ] ||
        fail "hard deleting a recovered profile left its data behind"
    mark_test softDeleteRecovery
}

exercise_update_rollback() {
    local update_dir key root archive guest manifest host_hash guest_hash
    local binary_before binary_after selection_before selection_after journal

    update_dir=$WORK/update-rollback
    mkdir -m 0700 "$update_dir"
    key=$update_dir/release-key
    ssh-keygen -q -t ed25519 -N '' -f "$key" ||
        fail "cannot create the isolated update rollback signing key"
    chmod 0600 "$key"

    root=$update_dir/hamn-v0.0.2-darwin-arm64
    cp -R "$ARTIFACT_ROOT" "$root" ||
        fail "cannot stage a temporary update host artifact"
    [ -f "$root/bin/hamn" ] && [ -f "$root/scripts/install-host.sh" ] &&
        [ -f "$root/scripts/update-host.sh" ] &&
        [ -d "$root/packaging/release" ] ||
        fail "temporary update host artifact is incomplete"
    cp "$key.pub" "$root/packaging/release/hamn-release.pub"
    cat >"$root/scripts/install-host.sh" <<'EOF'
#!/bin/bash
exit 77
EOF
    chmod 0755 "$root/scripts/install-host.sh"

    archive=$update_dir/host.tar.gz
    COPYFILE_DISABLE=1 tar -C "$update_dir" -czf "$archive" \
        "$(basename "$root")"
    guest=$update_dir/guest.img
    printf 'isolated signed rollback guest image\n' >"$guest"
    host_hash=$(sha256_file "$archive")
    guest_hash=$(sha256_file "$guest")
    manifest=$update_dir/manifest.json
    printf '%s' \
        '{"schemaVersion":1,"channel":"stable","version":"v0.0.2",' \
        '"compatibility":{"os":"darwin","architecture":"arm64","minimumMacOS":"13.0"},' \
        '"artifacts":{"host":{"url":"file://'"$archive"'","sha256":"'"$host_hash"'"},' \
        '"guestImage":{"url":"file://'"$guest"'","sha256":"'"$guest_hash"'"}}}' \
        >"$manifest"
    ssh-keygen -Y sign -f "$key" -n hamn-release "$manifest" >/dev/null ||
        fail "cannot sign the isolated update rollback manifest"

    binary_before=$(readlink "$HAMN") ||
        fail "cannot read the managed Hamn binary target before update rollback"
    selection_before=$(sha256_file "$TEST_HOME/.hamn/cache/guest-image.json")
    journal=$TEST_HOME/.hamn/cache/.hamn-update-transaction
    if env -i HOME="$TEST_HOME" PATH="$VALIDATOR_PATH" \
        HAMN_UPDATE_PUBLIC_KEY="$key.pub" \
        HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS=1 "$HAMN" update \
        --manifest "file://$manifest" >"$update_dir/update.out" \
        2>"$update_dir/update.err"; then
        fail "the deliberately failing signed host installer was accepted"
    fi
    grep -Fq 'host install failed; prior binary and guest image selection were restored' \
        "$update_dir/update.err" ||
        fail "signed update did not reach the failing host installer rollback"

    binary_after=$(readlink "$HAMN") ||
        fail "cannot read the managed Hamn binary target after update rollback"
    selection_after=$(sha256_file "$TEST_HOME/.hamn/cache/guest-image.json")
    [ "$binary_after" = "$binary_before" ] &&
        [ "$selection_after" = "$selection_before" ] ||
        fail "failing signed update changed the active binary or guest selection"
    [ ! -e "$journal" ] && [ ! -L "$journal" ] ||
        fail "failing signed update left an active recovery journal"
    mark_test updateRollback
}

await_http_response() {
    local port=$1 expected=$2
    "$PYTHON3" - "$port" "$expected" <<'PY'
import sys
import time
import urllib.request

url = "http://127.0.0.1:" + sys.argv[1] + "/"
expected = sys.argv[2].encode("utf-8")
deadline = time.monotonic() + 30
last = None
while time.monotonic() < deadline:
    try:
        with urllib.request.urlopen(url, timeout=2) as response:
            if response.read() == expected:
                raise SystemExit(0)
            last = "unexpected response"
    except Exception as error:
        last = str(error)
    time.sleep(0.1)
raise SystemExit("TCP published port did not become reachable: " + str(last))
PY
}

exercise_published_ports() {
    local mapping tcp_port udp_port
    run_docker --context hamn run -d --name hamn-e2e-tcp \
        -p 127.0.0.1::8080/tcp alpine:3.21 sh -ec \
        'mkdir -p /www; printf "hamn-tcp\n" >/www/index.html; exec busybox httpd -f -p 8080 -h /www' \
        >/dev/null
    mapping=$(run_docker --context hamn port hamn-e2e-tcp 8080/tcp)
    case "$mapping" in
    127.0.0.1:[0-9]*) tcp_port=${mapping##*:} ;;
    *) fail "Docker did not publish a loopback TCP port: $mapping" ;;
    esac
    await_http_response "$tcp_port" 'hamn-tcp\n'
    run_docker --context hamn rm -f hamn-e2e-tcp >/dev/null
    mark_test tcpPort

    run_docker --context hamn run -d --name hamn-e2e-udp \
        -p 127.0.0.1::19090/udp python:3.12-alpine python3 -u -c \
        'import socket; sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); sock.bind(("0.0.0.0", 19090)); data, peer = sock.recvfrom(1024); sock.sendto(b"hamn-udp:" + data, peer)' \
        >/dev/null
    mapping=$(run_docker --context hamn port hamn-e2e-udp 19090/udp)
    case "$mapping" in
    127.0.0.1:[0-9]*) udp_port=${mapping##*:} ;;
    *) fail "Docker did not publish a loopback UDP port: $mapping" ;;
    esac
    "$PYTHON3" - "$udp_port" <<'PY'
import socket
import sys
import time

port = int(sys.argv[1])
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.settimeout(0.2)
deadline = time.monotonic() + 30
last = None
while time.monotonic() < deadline:
    try:
        sock.sendto(b"probe", ("127.0.0.1", port))
        data, _ = sock.recvfrom(1024)
        if data == b"hamn-udp:probe":
            raise SystemExit(0)
        last = "unexpected response"
    except OSError as error:
        last = str(error)
raise SystemExit("UDP published port did not become reachable: " + str(last))
PY
    run_docker --context hamn wait hamn-e2e-udp >/dev/null
    run_docker --context hamn rm hamn-e2e-udp >/dev/null
    mark_test udpPort
}

exercise_amd64() {
    local architecture
    architecture=$(run_docker --context hamn run --rm --platform linux/amd64 \
        alpine:3.21 uname -m)
    case "$architecture" in
    x86_64|amd64) ;;
    *) fail "linux/amd64 container did not execute under binfmt: $architecture" ;;
    esac
    mark_test amd64
}

exercise_rosetta() {
    local profile=rosetta architecture

    run_hamn configure --profile "$profile" --rosetta true
    run_hamn start --profile "$profile" --template=false
    run_hamn ssh --profile "$profile" -- sudo test -x /mnt/hamn-rosetta/rosetta ||
        fail "Rosetta profile did not expose the Linux Rosetta runtime share"
    run_hamn ssh --profile "$profile" -- sudo grep -Fxq enabled \
        /proc/sys/fs/binfmt_misc/hamn-rosetta ||
        fail "Rosetta profile did not enable the hamn-rosetta binfmt handler"

    architecture=$(run_docker --context "hamn-$profile" run --rm \
        --platform linux/amd64 alpine:3.21 uname -m)
    case "$architecture" in
    x86_64|amd64) ;;
    *) fail "linux/amd64 container did not execute under Rosetta: $architecture" ;;
    esac

    run_hamn stop --profile "$profile"
    printf 'y\n' | run_hamn delete --profile "$profile" --data
    [ ! -e "$TEST_HOME/.hamn/$profile" ] &&
        [ ! -L "$TEST_HOME/.hamn/$profile" ] ||
        fail "hard deleting the Rosetta profile left runtime data behind"
    mark_test rosetta
}

exercise_compose() {
    local api_id mapping tcp_port udp_port
    COMPOSE_DIR=$WORK/compose
    mkdir -p "$COMPOSE_DIR/bind"
    printf 'compose-secret\n' >"$COMPOSE_DIR/secret.txt"
    printf 'compose-config\n' >"$COMPOSE_DIR/config.txt"
    cat >"$COMPOSE_DIR/Dockerfile" <<'EOF'
FROM alpine:3.21
COPY entrypoint.sh /entrypoint.sh
RUN chmod 0755 /entrypoint.sh
ENTRYPOINT ["/entrypoint.sh"]
EOF
    cat >"$COMPOSE_DIR/entrypoint.sh" <<'EOF'
#!/bin/sh
set -eu
if [ "$#" -gt 0 ]; then
    exec "$@"
fi
printf '%s\n' "$APP_ENV" >/workspace/environment.txt
cat /run/secrets/demo_secret >/workspace/secret.txt
cat /run/configs/demo.conf >/workspace/config.txt
printf 'named-volume\n' >/var/lib/hamn/named.txt
mkdir -p /www
printf 'compose-tcp\n' >/www/index.html
touch /tmp/healthy
echo api-ready
exec busybox httpd -f -p 8081 -h /www
EOF
    chmod 0755 "$COMPOSE_DIR/entrypoint.sh"
    cat >"$COMPOSE_DIR/compose.yaml" <<'EOF'
services:
  api:
    build: .
    environment:
      APP_ENV: "$COMPOSE_APP_ENV"
    ports:
      - "127.0.0.1::8081/tcp"
    volumes:
      - type: bind
        source: $COMPOSE_BIND_SOURCE
        target: /workspace
      - type: volume
        source: nameddata
        target: /var/lib/hamn
    secrets:
      - source: demo_secret
        target: demo_secret
    configs:
      - source: demo_config
        target: /run/configs/demo.conf
    healthcheck:
      test: ["CMD-SHELL", "test -f /tmp/healthy"]
      interval: 1s
      timeout: 1s
      retries: 20
  worker:
    image: alpine:3.21
    depends_on:
      api:
        condition: service_healthy
    environment:
      APP_ENV: "$COMPOSE_APP_ENV"
    command:
      - /bin/sh
      - -ec
      - |
        (getent hosts api >/dev/null || nslookup api >/dev/null)
        test "$APP_ENV" = compose-e2e
        echo worker-ready
        exec tail -f /dev/null
  udp:
    image: python:3.12-alpine
    ports:
      - "127.0.0.1::19091/udp"
    command:
      - python3
      - -u
      - -c
      - |
        import socket
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.bind(("0.0.0.0", 19091))
        data, peer = sock.recvfrom(1024)
        sock.sendto(b"compose-udp:" + data, peer)
  profile:
    image: alpine:3.21
    profiles: ["optional"]
    command: ["/bin/sh", "-ec", "echo profile-ready"]
  tty:
    image: alpine:3.21
    tty: true
    command: ["/bin/sh", "-ec", "test -t 0; test -t 1; echo tty-ready"]
volumes:
  nameddata: {}
secrets:
  demo_secret:
    file: ./secret.txt
configs:
  demo_config:
    file: ./config.txt
EOF

    run_compose build
    run_compose up -d --wait --wait-timeout 90
    api_id=$(run_compose ps -q api)
    [ -n "$api_id" ] &&
        [ "$(run_docker --context hamn inspect --format '{{.State.Health.Status}}' "$api_id")" = healthy ] ||
        fail "Compose did not wait for a healthy API service"
    grep -Fxq compose-e2e "$COMPOSE_DIR/bind/environment.txt" ||
        fail "Compose bind mount or environment propagation failed"
    grep -Fxq compose-secret "$COMPOSE_DIR/bind/secret.txt" ||
        fail "Compose secret mount failed"
    grep -Fxq compose-config "$COMPOSE_DIR/bind/config.txt" ||
        fail "Compose config mount failed"
    run_compose exec -T api /bin/cat /var/lib/hamn/named.txt |
        grep -Fxq named-volume ||
        fail "Compose named volume failed"
    run_compose logs worker | grep -Fq worker-ready ||
        fail "Compose network DNS or depends_on health ordering failed"
    run_compose logs api | grep -Fq api-ready ||
        fail "Compose logs failed"
    run_compose exec -T api /bin/cat /run/secrets/demo_secret |
        grep -Fxq compose-secret ||
        fail "Compose exec failed"
    run_compose run --rm -T api /bin/sh -ec \
        'test "$(cat /run/configs/demo.conf)" = compose-config' >/dev/null
    run_compose run --rm tty | grep -Fq tty-ready ||
        fail "Compose TTY service failed"
    run_compose --profile optional run --rm profile | grep -Fq profile-ready ||
        fail "Compose profile service failed"

    mapping=$(run_compose port api 8081)
    case "$mapping" in
    127.0.0.1:[0-9]*) tcp_port=${mapping##*:} ;;
    *) fail "Compose did not publish a loopback TCP port: $mapping" ;;
    esac
    await_http_response "$tcp_port" 'compose-tcp\n'
    mapping=$(run_compose port udp 19091)
    case "$mapping" in
    127.0.0.1:[0-9]*) udp_port=${mapping##*:} ;;
    *) fail "Compose did not publish a loopback UDP port: $mapping" ;;
    esac
    "$PYTHON3" - "$udp_port" <<'PY'
import socket
import sys
import time

port = int(sys.argv[1])
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.settimeout(0.2)
deadline = time.monotonic() + 30
last = None
while time.monotonic() < deadline:
    try:
        sock.sendto(b"probe", ("127.0.0.1", port))
        data, _ = sock.recvfrom(1024)
        if data == b"compose-udp:probe":
            raise SystemExit(0)
        last = "unexpected response"
    except OSError as error:
        last = str(error)
raise SystemExit("Compose UDP port did not become reachable: " + str(last))
PY
    run_compose down --volumes --remove-orphans
    if run_docker --context hamn volume inspect hamn-e2e-compose_nameddata \
        >/dev/null 2>&1; then
        fail "Compose down did not remove its named volume"
    fi
    mark_test compose
}

exercise_buildx() {
    local build_dir first second
    build_dir=$WORK/buildx
    mkdir -p "$build_dir"
    cat >"$build_dir/Dockerfile" <<'EOF'
FROM alpine:3.21 AS build
RUN printf 'buildx-cache-proof\n' >/artifact
FROM alpine:3.21
COPY --from=build /artifact /artifact
CMD ["/bin/cat", "/artifact"]
EOF
    run_docker --context hamn buildx inspect default |
        grep -Fq 'Driver: docker' ||
        fail "the Docker buildx driver is unavailable"
    run_docker --context hamn buildx build --builder default --load \
        --progress=plain --tag hamn-e2e-buildx:docker "$build_dir" \
        >"$build_dir/default-first.log" 2>&1
    run_docker --context hamn run --rm hamn-e2e-buildx:docker |
        grep -Fxq buildx-cache-proof ||
        fail "Docker buildx driver did not produce the multi-stage image"
    run_docker --context hamn buildx build --builder default --load \
        --progress=plain --tag hamn-e2e-buildx:docker "$build_dir" \
        >"$build_dir/default-second.log" 2>&1
    grep -Fq CACHED "$build_dir/default-second.log" ||
        fail "Docker buildx driver did not reuse the build cache"

    run_docker --context hamn buildx create --name hamn-e2e-builder \
        --driver docker-container --bootstrap >/dev/null
    run_docker --context hamn buildx inspect hamn-e2e-builder |
        grep -Fq 'Driver: docker-container' ||
        fail "the docker-container buildx driver is unavailable"
    first=$build_dir/container-first.log
    second=$build_dir/container-second.log
    run_docker --context hamn buildx build --builder hamn-e2e-builder --load \
        --progress=plain \
        --cache-to "type=local,dest=$build_dir/cache,mode=max" \
        --tag hamn-e2e-buildx:container "$build_dir" >"$first" 2>&1
    run_docker --context hamn buildx build --builder hamn-e2e-builder --load \
        --progress=plain \
        --cache-from "type=local,src=$build_dir/cache" \
        --tag hamn-e2e-buildx:container "$build_dir" >"$second" 2>&1
    grep -Fq CACHED "$second" ||
        fail "docker-container buildx driver did not reuse exported cache"
    run_docker --context hamn run --rm hamn-e2e-buildx:container |
        grep -Fxq buildx-cache-proof ||
        fail "docker-container buildx driver did not load the multi-stage image"
    run_docker --context hamn buildx rm hamn-e2e-builder >/dev/null
    mark_test buildx
}

exercise_docker_sdks() {
    local go_dir python_dir
    run_hamn env >"$WORK/hamn-env.sh"
    grep -Fxq "export DOCKER_HOST='unix://$TEST_HOME/.hamn/default/docker.sock'" \
        "$WORK/hamn-env.sh" ||
        fail "hamn env did not expose the current profile Docker socket"
    grep -Fxq "export TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE='/var/run/docker.sock'" \
        "$WORK/hamn-env.sh" ||
        fail "hamn env did not expose the Testcontainers socket override"
    grep -Fxq "export TESTCONTAINERS_HOST_OVERRIDE='host.docker.internal'" \
        "$WORK/hamn-env.sh" ||
        fail "hamn env did not expose the Testcontainers host override"

    go_dir=$WORK/docker-sdk-go
    mkdir -p "$go_dir"
    cat >"$go_dir/go.mod" <<'EOF'
module hamn-e2e/docker-sdk

go 1.22

require github.com/docker/docker v27.5.1+incompatible
EOF
    cat >"$go_dir/main.go" <<'EOF'
package main

import (
	"context"
	"fmt"

	"github.com/docker/docker/client"
)

func main() {
	client, err := client.NewClientWithOpts(
		client.FromEnv,
		client.WithAPIVersionNegotiation(),
	)
	if err != nil {
		panic(err)
	}
	defer client.Close()
	if _, err := client.Ping(context.Background()); err != nil {
		panic(err)
	}
	fmt.Println("docker-go-sdk-ready")
}
EOF
    run_sdk env GOMODCACHE="$WORK/go-mod-cache" GOCACHE="$WORK/go-cache" \
        GOPATH="$WORK/go-path" go -C "$go_dir" mod tidy
    run_sdk env GOMODCACHE="$WORK/go-mod-cache" GOCACHE="$WORK/go-cache" \
        GOPATH="$WORK/go-path" go -C "$go_dir" run . |
        grep -Fxq docker-go-sdk-ready ||
        fail "Go Docker SDK cannot connect through the Hamn Docker socket"
    mark_test dockerSdkGo

    python_dir=$WORK/docker-sdk-python
    "$PYTHON3" -m venv "$python_dir/venv" ||
        fail "Python venv is unavailable for Docker SDK validation"
    run_sdk "$python_dir/venv/bin/python" -m pip install \
        --disable-pip-version-check --no-input --cache-dir "$WORK/pip-cache" \
        'docker==7.1.0' >/dev/null
    cat >"$python_dir/docker_sdk.py" <<'PY'
import docker

client = docker.from_env()
container = None
try:
    client.ping()
    container = client.containers.run(
        "alpine:3.21", ["sh", "-ec", "exit 0"], detach=True
    )
    result = container.wait()
    if result["StatusCode"] != 0:
        raise RuntimeError("Docker SDK container failed")
finally:
    if container is not None:
        container.remove(force=True)
print("docker-python-sdk-ready")
PY
    run_sdk "$python_dir/venv/bin/python" "$python_dir/docker_sdk.py" |
        grep -Fxq docker-python-sdk-ready ||
        fail "Python Docker SDK cannot connect through the Hamn Docker socket"
    mark_test dockerSdkPython
}

exercise_testcontainers() {
    local go_dir python_dir java_dir node_dir
    go_dir=$WORK/testcontainers-go
    mkdir -p "$go_dir"
    cat >"$go_dir/go.mod" <<'EOF'
module hamn-e2e/testcontainers-go

go 1.22

require github.com/testcontainers/testcontainers-go v0.33.0
EOF
    cat >"$go_dir/main.go" <<'EOF'
package main

import (
	"context"
	"fmt"

	"github.com/testcontainers/testcontainers-go"
)

func main() {
	ctx := context.Background()
	container, err := testcontainers.GenericContainer(ctx,
		testcontainers.GenericContainerRequest{
			ContainerRequest: testcontainers.ContainerRequest{
				Image: "alpine:3.21",
				Cmd:   []string{"sh", "-ec", "while true; do sleep 1; done"},
			},
			Started: true,
		})
	if err != nil {
		panic(err)
	}
	defer func() {
		if err := container.Terminate(ctx); err != nil {
			panic(err)
		}
	}()
	fmt.Println("testcontainers-go-ready")
}
EOF
    run_sdk env GOMODCACHE="$WORK/testcontainers-go-mod-cache" \
        GOCACHE="$WORK/testcontainers-go-cache" \
        GOPATH="$WORK/testcontainers-go-path" \
        go -C "$go_dir" mod tidy
    run_sdk env GOMODCACHE="$WORK/testcontainers-go-mod-cache" \
        GOCACHE="$WORK/testcontainers-go-cache" \
        GOPATH="$WORK/testcontainers-go-path" \
        go -C "$go_dir" run . |
        grep -Fxq testcontainers-go-ready ||
        fail "Go Testcontainers cannot start through the Hamn Docker socket"
    mark_test testcontainersGo

    python_dir=$WORK/testcontainers-python
    "$PYTHON3" -m venv "$python_dir/venv" ||
        fail "Python venv is unavailable for Testcontainers validation"
    run_sdk "$python_dir/venv/bin/python" -m pip install \
        --disable-pip-version-check --no-input --cache-dir "$WORK/pip-cache" \
        'testcontainers==4.9.2' >/dev/null
    cat >"$python_dir/testcontainers.py" <<'PY'
from testcontainers.core.container import DockerContainer

container = DockerContainer("alpine:3.21").with_command(
    "sh -ec 'while true; do sleep 1; done'"
)
try:
    container.start()
    wrapped = container.get_wrapped_container()
    wrapped.reload()
    if wrapped.status != "running":
        raise RuntimeError("Python Testcontainers did not keep the container running")
finally:
    container.stop()
print("testcontainers-python-ready")
PY
    run_sdk "$python_dir/venv/bin/python" "$python_dir/testcontainers.py" |
        grep -Fxq testcontainers-python-ready ||
        fail "Python Testcontainers cannot start through the Hamn Docker socket"
    mark_test testcontainersPython

    java_dir=$WORK/testcontainers-java
    mkdir -p "$java_dir/src/main/java"
    cat >"$java_dir/pom.xml" <<'EOF'
<project xmlns="http://maven.apache.org/POM/4.0.0"
         xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
         xsi:schemaLocation="http://maven.apache.org/POM/4.0.0 https://maven.apache.org/xsd/maven-4.0.0.xsd">
  <modelVersion>4.0.0</modelVersion>
  <groupId>dev.hamn.e2e</groupId>
  <artifactId>testcontainers-java</artifactId>
  <version>1</version>
  <properties>
    <maven.compiler.release>17</maven.compiler.release>
  </properties>
  <dependencies>
    <dependency>
      <groupId>org.testcontainers</groupId>
      <artifactId>testcontainers</artifactId>
      <version>1.20.6</version>
    </dependency>
  </dependencies>
  <build>
    <plugins>
      <plugin>
        <groupId>org.codehaus.mojo</groupId>
        <artifactId>exec-maven-plugin</artifactId>
        <version>3.5.0</version>
      </plugin>
    </plugins>
  </build>
</project>
EOF
    cat >"$java_dir/src/main/java/Main.java" <<'EOF'
import org.testcontainers.containers.GenericContainer;
import org.testcontainers.utility.DockerImageName;

public final class Main {
    public static void main(String[] args) {
        try (GenericContainer<?> container = new GenericContainer<>(
                DockerImageName.parse("alpine:3.21"))
                .withCommand("sh", "-ec", "while true; do sleep 1; done")) {
            container.start();
            if (!container.isRunning()) {
                throw new IllegalStateException("Java Testcontainers did not start");
            }
        }
        System.out.println("testcontainers-java-ready");
    }
}
EOF
    run_sdk mvn -q -Dmaven.repo.local="$WORK/maven-repository" \
        -f "$java_dir/pom.xml" compile exec:java -Dexec.mainClass=Main |
        grep -Fxq testcontainers-java-ready ||
        fail "Java Testcontainers cannot start through the Hamn Docker socket"
    mark_test testcontainersJava

    node_dir=$WORK/testcontainers-node
    mkdir -p "$node_dir"
    run_sdk env npm_config_cache="$WORK/npm-cache" npm --prefix "$node_dir" \
        install --no-audit --no-fund --save-exact testcontainers@10.21.0 \
        >/dev/null
    cat >"$node_dir/index.cjs" <<'EOF'
const { GenericContainer } = require("testcontainers");

(async () => {
  const container = await new GenericContainer("alpine:3.21")
    .withCommand(["sh", "-ec", "while true; do sleep 1; done"])
    .start();
  try {
    if (!container.getId()) {
      throw new Error("Node Testcontainers did not return a container id");
    }
  } finally {
    await container.stop();
  }
  console.log("testcontainers-node-ready");
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
EOF
    run_sdk node "$node_dir/index.cjs" |
        grep -Fxq testcontainers-node-ready ||
        fail "Node Testcontainers cannot start through the Hamn Docker socket"
    mark_test testcontainersNode
}

exercise_kubernetes() {
    local moby_id
    mkdir -p "$TEST_HOME/.kube"
    chmod 0700 "$TEST_HOME/.kube"
    cat >"$TEST_HOME/.kube/config" <<'EOF'
apiVersion: v1
kind: Config
clusters:
- name: previous
  cluster:
    server: https://127.0.0.1:65535
users:
- name: previous
  user:
    token: previous-token
contexts:
- name: previous
  context:
    cluster: previous
    user: previous
current-context: previous
EOF
    chmod 0600 "$TEST_HOME/.kube/config"
    [ "$(env -i HOME="$TEST_HOME" PATH="$VALIDATOR_PATH" \
        "$KUBECTL" config current-context)" = previous ] ||
        fail "cannot establish an isolated pre-existing Kubernetes context"

    run_docker --context hamn run -d --name hamn-e2e-moby alpine:3.21 \
        sh -ec 'while true; do sleep 1; done' >/dev/null
    moby_id=$(run_docker --context hamn inspect --format '{{.Id}}' hamn-e2e-moby)
    run_hamn ssh -- sudo ctr -n moby containers list >"$WORK/moby-ctr.txt"
    grep -Fq "$moby_id" "$WORK/moby-ctr.txt" ||
        fail "Docker containers are not visible in the guest moby namespace"

    run_hamn kubernetes start
    run_hamn kubectl wait --for=condition=Ready node --all --timeout=120s
    run_hamn kubectl get deployment coredns --namespace kube-system \
        --no-headers >/dev/null
    [ "$(env -i HOME="$TEST_HOME" PATH="$VALIDATOR_PATH" \
        "$KUBECTL" config current-context)" = hamn ] ||
        fail "Hamn did not activate its owned Kubernetes context"
    run_hamn ssh -- sudo test -S /run/containerd/containerd.sock
    run_hamn ssh -- sudo ctr -n k8s.io containers list >"$WORK/k8s-ctr.txt"
    [ -s "$WORK/k8s-ctr.txt" ] ||
        fail "K3s workloads are not visible in the guest k8s.io namespace"
    run_hamn status --json >"$WORK/k3s-status.json"
    "$PYTHON3" - "$WORK/k3s-status.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    value = json.load(source)
if value["cri"] != {"ready": True, "namespace": "k8s.io"} or \
        value["kubernetes"] != {"enabled": True, "ready": True}:
    raise SystemExit("K3s or CRI readiness is incorrect")
PY
    run_hamn kubernetes delete
    [ "$(env -i HOME="$TEST_HOME" PATH="$VALIDATOR_PATH" \
        "$KUBECTL" config current-context)" = previous ] ||
        fail "Kubernetes delete did not restore the preceding context"
    [ "$(run_hamn kubernetes status)" = disabled ] ||
        fail "Kubernetes delete did not disable the profile"
    run_docker --context hamn rm -f hamn-e2e-moby >/dev/null
    mark_test k3s
}

exercise_uninstall() {
    local output=$WORK/uninstall.txt
    run_hamn stop
    if run_hamn uninstall </dev/null >"$output"; then
        fail "uninstall accepted EOF confirmation"
    fi
    if printf 'n\n' | run_hamn uninstall >>"$output"; then
        fail "uninstall accepted a non-y confirmation"
    fi
    [ -d "$TEST_HOME/.hamn" ] && [ -x "$HAMN" ] ||
        fail "uninstall removed data without an exact y confirmation"
    printf 'y\n' | run_hamn uninstall >>"$output"
    grep -Fq 'Hamn uninstall will remove the following managed paths:' "$output" ||
        fail "uninstall did not display its managed deletion plan"
    [ ! -e "$TEST_HOME/.hamn" ] && [ ! -L "$TEST_HOME/.hamn" ] &&
        [ ! -e "$HAMN" ] && [ ! -L "$HAMN" ] &&
        [ ! -e "$TEST_HOME/.local/share/hamn/src" ] &&
        [ ! -L "$TEST_HOME/.local/share/hamn/src" ] ||
        fail "uninstall did not remove isolated Hamn files and runtime data"
    mark_test uninstall
}

write_evidence() {
    local colima_after colima_binary_after colima_instances_after
    colima_after=$(colima_state_hash)
    [ "$COLIMA_BEFORE_HASH" = "$colima_after" ] ||
        fail "Colima state changed while testing the isolated Hamn candidate"
    colima_binary_after=$(colima_binary_hash)
    [ "$COLIMA_BINARY_BEFORE_HASH" = "$colima_binary_after" ] ||
        fail "Colima executable changed while testing the isolated Hamn candidate"
    colima_instances_after=$(colima_instance_inventory_hash) ||
        fail "cannot read the existing Colima VM/profile inventory after testing"
    [ "$COLIMA_INSTANCES_BEFORE_HASH" = "$colima_instances_after" ] ||
        fail "Colima VM/profile inventory changed while testing the isolated Hamn candidate"
    mark_test colimaCoexistence
    "$PYTHON3" - "$WORK/tests.txt" "$HAMN_E2E_OUTPUT" \
        "$HAMN_CANDIDATE_JSON" "$HOST_ARTIFACT_HASH" "$GUEST_ARTIFACT_HASH" \
        "$COLIMA_BEFORE_HASH" "$colima_after" "$COLIMA_BINARY_BEFORE_HASH" \
        "$colima_binary_after" "$COLIMA_INSTANCES_BEFORE_HASH" \
        "$colima_instances_after" <<'PY'
import hashlib
import json
import re
import sys

(tests_path, output_path, candidate_path, host_hash, guest_hash, before_hash,
 after_hash, binary_before_hash, binary_after_hash, instances_before_hash,
 instances_after_hash) = sys.argv[1:]
expected = {
    "lifecycle", "multiProfile", "staleSocket", "dockerContextRestore",
    "mount", "network", "tcpPort", "udpPort", "amd64", "rosetta", "dockerCli",
    "compose", "buildx", "dockerSdkGo", "dockerSdkPython",
    "testcontainersJava", "testcontainersGo", "testcontainersPython",
    "testcontainersNode", "k3s", "updateRollback", "softDeleteRecovery",
    "hardDelete",
    "uninstall", "colimaCoexistence",
}
with open(tests_path, encoding="utf-8") as source:
    actual = [line.rstrip("\n") for line in source]
if len(actual) != len(set(actual)) or set(actual) != expected:
    missing = sorted(expected - set(actual))
    extra = sorted(set(actual) - expected)
    raise SystemExit("incomplete physical tests; missing=" + repr(missing) +
                     " extra=" + repr(extra))
with open(candidate_path, "rb") as source:
    candidate_hash = hashlib.sha256(source.read()).hexdigest()
for name, value in {
        "candidate": candidate_hash, "host": host_hash, "guest": guest_hash,
        "colima before": before_hash, "colima after": after_hash,
        "colima binary before": binary_before_hash,
        "colima binary after": binary_after_hash,
        "colima instances before": instances_before_hash,
        "colima instances after": instances_after_hash}.items():
    if not re.fullmatch(r"[0-9a-f]{64}", value):
        raise SystemExit("invalid " + name + " digest")
evidence = {
    "schemaVersion": 1,
    "kind": "hamn-physical-e2e",
    "passed": True,
    "tests": {name: True for name in sorted(expected)},
    "provenance": {
        "candidateJsonSha256": candidate_hash,
        "hostArtifactSha256": host_hash,
        "guestImageSha256": guest_hash,
    },
    "colima": {
        "beforeSha256": before_hash,
        "afterSha256": after_hash,
        "binaryBeforeSha256": binary_before_hash,
        "binaryAfterSha256": binary_after_hash,
        "instancesBeforeSha256": instances_before_hash,
        "instancesAfterSha256": instances_after_hash,
    },
}
with open(output_path, "x", encoding="utf-8", newline="\n") as output:
    json.dump(evidence, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
}

# physical-e2e main entry point
if [ "$#" -gt 0 ]; then
    if [ "$#" = 1 ] && { [ "$1" = --help ] || [ "$1" = -h ]; }; then
        usage
        exit 0
    fi
    if [ "$#" = 1 ] && [ "$1" = --preflight ]; then
        require_validator_environment
        echo "physical validator preflight passed without installing or starting Hamn"
        exit 0
    fi
    usage >&2
    exit 2
fi

require_environment
read_candidate
prepare_workspace
trap cleanup EXIT
COLIMA_BEFORE_HASH=$(colima_state_hash)
COLIMA_BINARY_BEFORE_HASH=$(colima_binary_hash)
COLIMA_INSTANCES_BEFORE_HASH=$(colima_instance_inventory_hash) ||
    fail "an existing readable Colima VM/profile inventory is required"
extract_candidate
install_candidate
stage_candidate_guest
exercise_lifecycle_and_core_docker
exercise_multi_profile_and_delete
exercise_soft_delete_recovery
exercise_update_rollback
exercise_published_ports
exercise_amd64
exercise_rosetta
exercise_compose
exercise_buildx
exercise_docker_sdks
exercise_testcontainers
exercise_kubernetes
exercise_uninstall
write_evidence
echo "validated physical Hamn candidate without mutating Colima"
