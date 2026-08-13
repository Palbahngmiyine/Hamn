#!/bin/bash
# Strict config.yaml and profile selection checks. No VM is started.
set -euo pipefail

HAMN="${HAMN:-build/hamn}"
WORK=$(mktemp -d /tmp/hamn-profile-yaml.XXXXXX)
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

run_hamn() {
    HOME="$WORK" "$HAMN" "$@"
}

expect_load_failure() {
    local profile=$1
    if run_hamn status "$profile" >"$WORK/out" 2>"$WORK/err"; then
        echo "FAIL: malformed $profile config was accepted" >&2
        exit 1
    fi
    grep -q 'cannot load profile' "$WORK/err"
}

write_profile_yaml() {
    local profile=$1
    shift
    mkdir -p "$WORK/.hamn/$profile"
    printf '%s\n' "$@" >"$WORK/.hamn/$profile/config.yaml"
}

# The selection order is --profile, positional profile, HAMN_PROFILE, default.
HOME="$WORK" HAMN_PROFILE=from-env "$HAMN" status --json >"$WORK/status-env.json"
grep -q '"profile":"from-env"' "$WORK/status-env.json"
run_hamn status --json >"$WORK/status-default.json"
grep -q '"profile":"default"' "$WORK/status-default.json"
run_hamn status -p from-flag from-positional --json >"$WORK/status-flag.json"
grep -q '"profile":"from-flag"' "$WORK/status-flag.json"
run_hamn status from-positional --json >"$WORK/status-positional.json"
grep -q '"profile":"from-positional"' "$WORK/status-positional.json"
run_hamn template from-positional >"$WORK/template.yaml"
grep -q '^mountHome: true$' "$WORK/template.yaml"
grep -q '^mountInotify: false$' "$WORK/template.yaml"
run_hamn env from-positional >"$WORK/env.sh"
grep -Fq "DOCKER_HOST='unix://$WORK/.hamn/from-positional/docker.sock'" \
    "$WORK/env.sh"
grep -q "TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE='/var/run/docker.sock'" \
    "$WORK/env.sh"
grep -q "TESTCONTAINERS_HOST_OVERRIDE='host.docker.internal'" "$WORK/env.sh"

# CLI resource changes persist to a private, atomic YAML profile.
run_hamn configure -p development --cpu 6 --memory 8 --disk 80 --output json >"$WORK/configure.json"
CONFIG="$WORK/.hamn/development/config.yaml"
[ -f "$CONFIG" ] || { echo "FAIL: config.yaml was not written" >&2; exit 1; }
[ "$(stat -f '%Lp' "$CONFIG")" = 600 ] || {
    echo "FAIL: config.yaml permissions are not 0600" >&2
    exit 1
}
[ ! -e "$CONFIG.tmp" ] || { echo "FAIL: stale config temp file" >&2; exit 1; }
grep -q '^cpus: 6$' "$CONFIG"
grep -q '^memoryMiB: 8192$' "$CONFIG"
grep -q '^diskGiB: 80$' "$CONFIG"
run_hamn status development --json >"$WORK/development.json"
grep -q '"dockerContext":"hamn-development"' "$WORK/development.json"

# Every supported profile setting can be changed through the CLI and survives
# the strict YAML round trip. These checks do not start a VM.
mkdir -p "$WORK/mount-source" "$WORK/external-source"
run_hamn configure -p advanced \
    --mount-home false \
    --home-read-only false \
    --mount-inotify true \
    --docker-daemon-json '{"features":{"buildkit":true}}' \
    --kubernetes true \
    --rosetta true \
    --nested-virtualization true \
    --ssh-agent true \
    --clear-mounts \
    --mount "$WORK/mount-source:/workspace:rw" \
    --mount "$WORK/external-source:/external:ro" \
    --clear-provision \
    --provision-hook 'system:30:fail:echo ready' \
    --provision-hook 'user:10:warn:echo user:ready' \
    --output json >"$WORK/advanced.json"
ADVANCED="$WORK/.hamn/advanced/config.yaml"
grep -q '"mountHome":false' "$WORK/advanced.json"
grep -q '"mountInotify":true' "$WORK/advanced.json"
grep -q '"kubernetesEnabled":true' "$WORK/advanced.json"
grep -q '^mountHome: false$' "$ADVANCED"
grep -q '^mountInotify: true$' "$ADVANCED"
if grep -q '^network:' "$ADVANCED"; then
    echo "FAIL: shared NAT-only profile serialized a network selector" >&2
    exit 1
fi
grep -Fq '  daemonJson: "{\"features\":{\"buildkit\":true}}"' "$ADVANCED"
grep -q '^  enabled: true$' "$ADVANCED"
grep -q '^rosetta: true$' "$ADVANCED"
grep -q '^nestedVirtualization: true$' "$ADVANCED"
grep -q '^sshAgent: true$' "$ADVANCED"
grep -Fq "  - location: \"$WORK/mount-source\"" "$ADVANCED"
grep -Fq "  - location: \"$WORK/external-source\"" "$ADVANCED"
grep -q '    writable: true$' "$ADVANCED"
grep -q '    writable: false$' "$ADVANCED"
grep -q '  - stage: "system"' "$ADVANCED"
grep -q '  - stage: "user"' "$ADVANCED"
grep -q '    mode: warn$' "$ADVANCED"

# Invalid CLI values fail before replacing the last valid YAML file.
cp "$ADVANCED" "$WORK/advanced.before.yaml"
for args in \
    '--mount relative:/guest:rw' \
    "--mount $WORK/mount-source:/guest:invalid" \
    '--provision-hook invalid:30:fail:echo ready' \
    '--provision-hook system:0:fail:echo ready' \
    '--docker-daemon-json [' \
    '--docker-daemon-json []' \
    '--docker-daemon-json {"containerd":"/other.sock"}' \
    '--docker-daemon-json {"features":{"buildkit":false}}' \
    '--docker-daemon-json {"debug":true,"debug":false}' \
    '--mount-home maybe' \
    '--mount-inotify true --mount-inotify false' \
    '--cpu 2 --cpu 3' \
    '--network shared' \
    '--network-interface en0'; do
    if run_hamn configure -p advanced $args >"$WORK/invalid.out" \
        2>"$WORK/invalid.err"; then
        echo "FAIL: invalid configure arguments were accepted: $args" >&2
        exit 1
    fi
    cmp "$ADVANCED" "$WORK/advanced.before.yaml"
done

# Removed network flags cannot create a profile either.
if run_hamn configure -p removed-network-flag --network shared \
    >"$WORK/network-flag.out" 2>"$WORK/network-flag.err"; then
    echo "FAIL: removed --network flag was accepted" >&2
    exit 1
fi
[ ! -e "$WORK/.hamn/removed-network-flag/config.yaml" ] || {
    echo "FAIL: removed --network flag wrote a profile" >&2
    exit 1
}

# mountInotify needs one writable share and must not publish a partial config.
if run_hamn configure -p no-writable-inotify --mount-home false \
    --mount-inotify true >"$WORK/mount-inotify-flag.out" \
    2>"$WORK/mount-inotify-flag.err"; then
    echo "FAIL: mountInotify without a writable share was accepted" >&2
    exit 1
fi
[ ! -e "$WORK/.hamn/no-writable-inotify/config.yaml" ] || {
    echo "FAIL: invalid mountInotify configuration was written" >&2
    exit 1
}

# The strict parser rejects every unsupported YAML construct.
write_profile_yaml unknown-key 'cpus: 4' 'unknown: true'
expect_load_failure unknown-key
write_profile_yaml duplicate-key 'cpus: 4' 'cpus: 5'
expect_load_failure duplicate-key
write_profile_yaml yaml-anchor 'cpus: &cpu 4'
expect_load_failure yaml-anchor
write_profile_yaml yaml-alias 'cpus: *cpu'
expect_load_failure yaml-alias
write_profile_yaml yaml-tag 'cpus: !!int 4'
expect_load_failure yaml-tag
write_profile_yaml yaml-merge 'base: &base { cpus: 4 }' '<<: *base'
expect_load_failure yaml-merge
write_profile_yaml quoted-bool 'mountHome: "true"'
expect_load_failure quoted-bool
write_profile_yaml wrong-type 'mounts: true'
expect_load_failure wrong-type
write_profile_yaml invalid-mount 'mounts:' '  - location: relative' '    mountPoint: /workspace' '    writable: false'
expect_load_failure invalid-mount
write_profile_yaml invalid-hook 'provision:' '  - command: echo ready' '    stage: invalid' '    timeoutSeconds: 60' '    mode: fail'
expect_load_failure invalid-hook
write_profile_yaml removed-network-key 'network:' '  mode: shared'
cp "$WORK/.hamn/removed-network-key/config.yaml" "$WORK/removed-network-key.before.yaml"
expect_load_failure removed-network-key
cmp "$WORK/.hamn/removed-network-key/config.yaml" \
    "$WORK/removed-network-key.before.yaml"
write_profile_yaml quoted-mount-inotify 'mountInotify: "true"'
expect_load_failure quoted-mount-inotify
write_profile_yaml no-writable-mount-inotify 'mountHome: false' \
    'mountInotify: true'
expect_load_failure no-writable-mount-inotify
write_profile_yaml invalid-docker-json 'docker:' '  daemonJson: "["'
expect_load_failure invalid-docker-json
write_profile_yaml invalid-docker-key 'docker:' '  daemonJson: "{\"containerd\":\"/other.sock\"}"'
expect_load_failure invalid-docker-key
# Legacy runtime profiles cannot be converted or started implicitly.
mkdir -p "$WORK/.hamn/legacy"
printf '%s\n' 'runtime=containerd' >"$WORK/.hamn/legacy/hamn.conf"
if run_hamn status legacy >"$WORK/legacy.out" 2>"$WORK/legacy.err"; then
    echo "FAIL: legacy runtime profile was accepted" >&2
    exit 1
fi
grep -q 'cannot load profile' "$WORK/legacy.err"
[ ! -e "$WORK/.hamn/legacy/config.yaml" ] || {
    echo "FAIL: legacy runtime profile was converted" >&2
    exit 1
}
if run_hamn start --profile legacy >"$WORK/legacy-start.out" \
    2>"$WORK/legacy-start.err"; then
    echo "FAIL: legacy runtime profile started" >&2
    exit 1
fi
grep -Fq 'Hamn will not convert it' "$WORK/legacy-start.err"
grep -Fq 'hamn delete --data --profile legacy' "$WORK/legacy-start.err"
grep -Fq 'hamn start --profile legacy' "$WORK/legacy-start.err"
if run_hamn delete --profile legacy >"$WORK/legacy-delete.out" \
    2>"$WORK/legacy-delete.err"; then
    echo "FAIL: legacy profile soft-delete was accepted" >&2
    exit 1
fi
grep -Fq 'use hamn delete --data --profile legacy to remove it' \
    "$WORK/legacy-delete.err"
printf 'y\n' | HOME="$WORK" "$HAMN" delete --data --profile legacy \
    >"$WORK/legacy-delete-data.out" 2>"$WORK/legacy-delete-data.err"
[ ! -e "$WORK/.hamn/legacy" ] || {
    echo "FAIL: legacy profile delete --data did not remove its data" >&2
    exit 1
}

# The former public runtime clients are no longer dispatchable.
for command in docker nerdctl; do
    if run_hamn "$command" >"$WORK/$command.out" 2>"$WORK/$command.err"; then
        echo "FAIL: hamn $command remains public" >&2
        exit 1
    fi
    grep -q "unknown command '$command'" "$WORK/$command.err"
done
if run_hamn start --runtime containerd >"$WORK/runtime.out" 2>"$WORK/runtime.err"; then
    echo "FAIL: start accepted --runtime" >&2
    exit 1
fi
if run_hamn start --template=invalid >"$WORK/template-invalid.out" \
    2>"$WORK/template-invalid.err"; then
    echo "FAIL: start accepted an invalid template mode" >&2
    exit 1
fi
grep -q -- '--template must be true or false' "$WORK/template-invalid.err"

# Soft deletion preserves data, while hard deletion requires exactly lowercase y.
run_hamn configure -p delete-me --cpu 2 --memory 2 --disk 60
mkdir -p "$WORK/.hamn/delete-me"
truncate -s 4096 "$WORK/.hamn/delete-me/disk.img"
run_hamn delete -p delete-me >"$WORK/soft-delete.out"
[ -f "$WORK/.hamn/delete-me/config.yaml" ]
[ -f "$WORK/.hamn/delete-me/disk.img" ]
[ -f "$WORK/.hamn/delete-me/deleted" ]
run_hamn list >"$WORK/list.out"
if grep -q 'delete-me' "$WORK/list.out"; then
    echo "FAIL: soft-deleted profile is listed" >&2
    exit 1
fi
if printf 'n\n' | HOME="$WORK" "$HAMN" delete -p delete-me --data >"$WORK/data-n.out" 2>"$WORK/data-n.err"; then
    echo "FAIL: delete --data accepted non-y confirmation" >&2
    exit 1
fi
[ -d "$WORK/.hamn/delete-me" ]
printf 'y\n' | HOME="$WORK" "$HAMN" delete -p delete-me --data >"$WORK/data-y.out" 2>"$WORK/data-y.err"
[ ! -e "$WORK/.hamn/delete-me" ] || {
    echo "FAIL: delete --data did not remove the profile" >&2
    exit 1
}

echo "PASS: strict YAML profile, selection, and deletion gates"
