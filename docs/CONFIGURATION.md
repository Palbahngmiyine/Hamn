# Configuration

This is the canonical English configuration reference. See
[CONFIGURATION.ko.md](CONFIGURATION.ko.md) for Korean.

## Profile selection and location

Profile state lives under `~/.hamn/<profile>/`, with mode `0700` directories.
Headless VM and Docker operations require an explicit `--profile`; `vm list`
needs no profile. The TUI maintains its own selection, initially `default`.
There is no positional-profile or `HAMN_PROFILE` fallback in the public API.

Profile names may contain only letters, digits, `_`, and `-`. `cache`, `.` and
`..` are not valid profile names.

`~/.hamn/<profile>/config.yaml` stores profile configuration. The TUI default
workspace is stored separately in `~/.hamn/tui.json`; see [TUI preferences](TUI.md). The
file is written atomically with mode `0600`. A legacy `hamn.conf` containing
`runtime=containerd` or `runtime=hamn` causes the profile to fail closed; Hamn
does not convert legacy runtime data in place.

## Editing configuration

```sh
hamn --headless vm create --profile work --cpu 4 --memory 4 --yes
hamn --headless vm configure --profile work --cpu 6 --memory 8 --disk 80 --yes
hamn --headless vm start --profile work --yes
```

`configure` changes stopped profiles only. Existing VM disks are not shrunk.
For advanced settings, edit `~/.hamn/<profile>/config.yaml` while the VM is
stopped. Resource-only updates preserve mounts, Docker daemon settings, Rosetta,
and existing provisioning hooks. In the TUI, `v` then `c` opens the resource
configuration command. Native `kubectl edit` uses the installed editor in the PTY.

## YAML schema

The following is the complete generated template:

```yaml
cpus: 4
memoryMiB: 4096
diskGiB: 60
mountHome: true
homeReadOnly: false
mountInotify: false
docker:
  daemonJson: ""
rosetta: false
nestedVirtualization: false
sshAgent: false
mounts: []
provision: []
```

The parser accepts exactly one YAML document. It rejects duplicate or unknown
keys, aliases, anchors, tags, merge keys, non-plain booleans and integers,
wrong collection types, and invalid paths. Do not depend on YAML implicit type
coercion.

| Key | Type and default | Meaning |
| --- | --- | --- |
| `cpus` | positive integer, `4` | VM CPU count. |
| `memoryMiB` | positive integer, `4096` | VM memory in MiB. |
| `diskGiB` | positive integer, `60` | Guest disk capacity; only growth is allowed. |
| `mountHome` | boolean, `true` | Expose the user's home directory via virtiofs. |
| `homeReadOnly` | boolean, `false` | Make the home share read-only; invalid when `mountHome` is false. |
| `mountInotify` | boolean, `false` | Experimental best-effort bridge for existing files in writable virtiofs shares; it requires at least one writable share. |
| `docker.daemonJson` | string containing one JSON object, `""` | Extra Docker daemon settings that do not replace Hamn-managed boundaries. |
| `rosetta` | boolean, `false` | Request Apple Linux Rosetta translation; available only when the host supports it. |
| `nestedVirtualization` | boolean, `false` | Request nested virtualization; requires macOS 15+, an M3-or-later Mac, and the framework capability check. |
| `sshAgent` | boolean, `false` | Forward the user's SSH agent into the Hamn SSH session only. |
| `mounts` | sequence, at most 16 entries | Additional virtiofs shares. |
| `provision` | sequence, at most 16 entries | Lifecycle hooks. |

## Docker daemon JSON

`docker.daemonJson` must decode as a strict JSON object with unique keys. It is
merged into guest `/etc/docker/daemon.json` after Hamn reserves its Docker and
network settings. Hamn rejects or later fails the guest transaction when a user
attempts to replace any of these managed boundaries:

```text
containerd, host-gateway-ip, hosts, data-root, exec-root,
dns, bip, bridge, fixed-cidr, default-address-pools
```

If `features.buildkit` appears it must be `true`; Hamn keeps BuildKit enabled.
The guest always sets its system containerd socket, Docker bridge DNS, and
`host.docker.internal` gateway itself. A configuration error leaves the
previous guest transaction recoverable instead of silently accepting a
partially applied daemon.

## Mounts

Each additional mount has this schema:

```yaml
mounts:
  - location: "/Users/<your-user>/project"
    mountPoint: "/workspace/project"
    writable: true
  - location: "/Volumes/reference-data"
    mountPoint: "/reference-data"
    writable: false
```

`location` and `mountPoint` are required absolute, normalized paths. `writable`
defaults to false. Mount points must be unique. Before launch, Hamn checks that
the host source is an owned, non-symlink directory. A writable source must be
inside the canonical `$HOME`; a source outside `$HOME` must remain read-only.
Hamn does not automatically mount the SSH agent into containers, including
when `sshAgent` is enabled.

`mountInotify` is off by default. When enabled, Hamn watches writable host
shares with macOS FSEvents and asks the guest agent to refresh the timestamp of
the corresponding existing regular file. This produces Linux `IN_ATTRIB` and
`IN_CLOSE_WRITE` events without rewriting file contents. It intentionally does
not promise events
for newly created files, deleted or renamed paths, directories, symlinks, or
dropped/coalesced FSEvents records. The agent rejects path traversal, read-only
shares, symlinks, and non-regular files. Ordinary virtiofs writes remain the
only guaranteed host-to-guest file-change behavior.

## Network

Every profile uses Virtualization.framework shared NAT: the guest has a private
address, published TCP ports use SSH ControlMaster, and published UDP ports use
a bounded host process. Hamn repairs forwarding state transactionally if setup
is interrupted. Network attachment is not a profile setting: the strict YAML
schema rejects `network`, and `configure` has no `--network` or
`--network-interface` option. Hamn does not provide a LAN-reachable guest
address in 0.0.1.

Guest Docker networks resolve `host.docker.internal`. `host.hamn.internal` is a
0.0.1 compatibility alias; successful guest Docker configuration warns that it
will be removed in the next release. Hamn does not touch host
`/var/run/docker.sock`.

## Kubernetes

Kubernetes uses external kubeconfig contexts, independently of a Hamn VM:

```sh
hamn --headless k8s contexts list
hamn --headless k8s pods list --context dev --namespace default
```

`--kubeconfig` takes precedence over `KUBECONFIG` and the default `~/.kube/config`.
Selection does not modify the source files. See [API](API.md) for authentication
and explicit mutation targets. The legacy `kubernetes` YAML mapping is read
only for K3s retirement and is removed after successful cleanup. It is not a
new-profile setting. K3s cluster data and dedicated volumes are deleted;
Docker data and the original kubeconfig are preserved.

## Provisioning hooks

Each hook has `stage`, `command`, optional `timeoutSeconds`, and optional
`mode`:

```yaml
provision:
  - stage: "system"
    command: "apt-get update"
    timeoutSeconds: 120
    mode: fail
  - stage: "ready"
    command: "echo application-ready"
    timeoutSeconds: 30
    mode: warn
```

The valid stages run in order `system`, `user`, `after-boot`, `ready`.
`system`, `after-boot`, and `ready` run with guest root privileges; `user` runs
as the guest `hamn` user. The timeout must be 1–3600 seconds and defaults to
60. `fail` is the default and stops startup; `warn` records a failure then
continues. Logs record only redacted metadata, not hook commands or output.

## Docker contexts and SDK environment

Hamn does not create, activate, or restore external Docker contexts. Obtain
connection data from `hamn --headless vm env --profile work`, or select the
profile socket directly:

```sh
export DOCKER_HOST="unix://$HOME/.hamn/work/docker.sock"
export TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE=/var/run/docker.sock
export TESTCONTAINERS_HOST_OVERRIDE=host.docker.internal
```

Docker CLI, Compose, buildx, and SDK clients share the public Docker socket.
There is no public containerd socket.
