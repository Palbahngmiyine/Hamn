# Colima compatibility

This is the canonical English compatibility reference. See
[COLIMA-COMPATIBILITY.ko.md](COLIMA-COMPATIBILITY.ko.md) for Korean.

Hamn is designed as a Docker-only, Apple-Silicon macOS alternative for local
development. It is not a `colima` alias and does not reuse Colima's VM, profiles,
configuration, Docker contexts, sockets, or installation. Colima itself
supports a broader host/runtime matrix; see the [Colima
project](https://github.com/abiosoft/colima).

## Command mapping

| Colima intent | Hamn command | Notes |
| --- | --- | --- |
| Start default instance | `hamn start` | Starts the default Docker-only profile. |
| Start named instance | `hamn start --profile work` | Profile resolution also supports positional name and `HAMN_PROFILE`. |
| Stop instance | `hamn stop --profile work` | Restores a prior Docker context only when Hamn activated it. |
| Delete VM but retain data | `hamn delete --profile work` | Soft delete preserves the profile disk. |
| Delete all profile data | `hamn delete --profile work --data` | Prints target and disk allocation; requires exact interactive `y`. |
| Show instance status | `hamn status --profile work --json` | Separates Docker API, CRI, and K3s readiness. |
| List instances | `hamn list` | Shows active Hamn profiles only. |
| Configure CPU / memory / disk | `hamn configure --cpu 6 --memory 8 --disk 80` | Configuration persists in profile YAML. |
| Edit profile settings | `hamn start --edit --profile work` | Uses `$EDITOR` and strict YAML validation. |
| Print defaults | `hamn template` | Prints a copyable profile YAML template. |
| Open a guest shell | `hamn ssh --profile work` | SSH is Hamn's host-to-guest control transport. |
| Docker client environment | `eval "$(hamn env --profile work)"` | Intended for SDKs and Testcontainers. |
| Enable local Kubernetes | `hamn kubernetes start --profile work` | Explicit, profile-local K3s lifecycle. |
| Use local Kubernetes | `hamn kubectl --profile work -- get nodes` | Uses only the selected profile kubeconfig. |
| Configure amd64 translation | `hamn configure --rosetta true` | Default is binfmt; Rosetta is opt-in. |
| Remove Hamn | `hamn uninstall` | Shows managed install and runtime data, then requires exact `y`. |

Run normal Docker tooling after Hamn starts:

~~~sh
hamn start --profile work
docker context show
docker compose up -d
docker buildx build --load -t example .
docker run --rm alpine uname -m
~~~

The default Hamn Docker context is `hamn`; named profiles use `hamn-<profile>`.
The Docker endpoint is `~/.hamn/<profile>/docker.sock`, not host
`/var/run/docker.sock`.

## Intentional differences

Hamn accepts only the Docker runtime publicly. It does not offer Colima's
runtime switching, `colima nerdctl` UX, Incus integration, Linux hosts, Intel Macs, GPU/AI
workloads, or multi-node Kubernetes in 0.0.1.

Hamn exposes Docker Engine API through the profile socket. It intentionally
does not expose guest `containerd` or CRI sockets to macOS. Docker uses the guest
`moby` namespace; optional K3s uses the same system containerd through CRI in
`k8s.io`. See [Architecture](ARCHITECTURE.md).

The only supported network mode is shared NAT; Hamn provides no per-profile
bridged or interface selection. Hamn manages Docker-published TCP ports with
SSH ControlMaster and UDP ports with a bounded relay.

## Colima coexistence

Hamn's coexistence validation and release procedures must leave Colima
untouched. In particular, they must not:

- start, stop, delete, or edit a Colima VM or profile;
- change a Colima Docker context or socket;
- install, update, uninstall, or alias Colima;
- reuse the Colima VM disk, SSH configuration, or runtime state.

## Migration outline

1. Record the existing Docker context and leave Colima running or stopped as it
   was.
2. Install Hamn without replacing the `docker` executable.
3. Start a named Hamn profile and confirm `docker context show` is
   `hamn-<profile>`.
4. Validate the project's Docker CLI, Compose, buildx, SDK, and Testcontainers
   workflow against the Hamn profile.
5. Stop Hamn and verify the original Docker context has been restored.
6. Compare Colima state before and after. If any Colima state changed, stop and
   investigate before continuing.

Do not delete or modify an existing Colima setup as part of a Hamn migration.
