# Hamn

Hamn manages a Linux VM, its Docker Engine, and external Kubernetes clusters
from one macOS executable. Run `hamn` for the Ratatui terminal interface or
use `hamn --headless` for JSON and NDJSON automation.

## Requirements

- Apple Silicon Mac with macOS 13 or later.
- A signed Hamn guest image for VM and Docker operations.
- A kubeconfig for Kubernetes operations. These work independently of the VM.

TUI container browsing requires Docker CLI; Kubernetes browsing requires kubectl.
Headless profile Docker and Kubernetes SDK operations do not require these CLIs;
external Docker `--context` operations require Docker CLI. Compose, buildx, and plugins
remain external installations. Docker Desktop is not required.

## Install

```sh
curl -fsSL --proto '=https' --tlsv1.2 \
  https://github.com/Palbahngmiyine/Hamn/releases/latest/download/install.sh \
  | /bin/bash
```

It downloads Hamn and its Linux guest image, verifies both, and installs the
`hamn` command in `~/.local/bin`. Example output:

```text
Installing Hamn 0.1.2 for Apple Silicon macOS...
Downloading Hamn 0.1.2 (4.4 MiB)...
Downloading guest image  100%  1.1 GiB / 1.1 GiB  11.8 MiB/s
Installing...
Installed Hamn 0.1.2.
Run hamn to get started. Update later with hamn upgrade.
```

If `~/.local/bin` is not on your PATH, the installer prints the one line to add
for your shell. It needs only macOS system tools; Python, Homebrew, Rust, and
Xcode Command Line Tools are not required for installation or updates.
Hamn 0.1.2 and earlier cannot update themselves to this release: run the
installer above once, which migrates the existing installation in place (see
[installation](docs/INSTALLATION.md#installed-layout-and-earlier-installations)).

For source builds, see [Development](docs/DEVELOPMENT.md).
Signed release installation is described in [release setup](docs/RELEASE-SETUP.md).

## Terminal interface

Choose Containers or Kubernetes on first launch. Tab switches workspace; `,`
changes the saved default. Enter `:ps`, `:docker ps -a`, `:images`, `:get pods -A`,
or `:kubectl get deployments -n dev`. Ordinary lists become selectable tables;
other commands run in the embedded terminal with their original CLI semantics.
`e` selects the environment/context; `v` opens VM controls only for Hamn profiles.
See [workspace and command guide](docs/TUI.md) for actions, settings, and cancellation.

## Headless interface

```sh
hamn --headless capabilities
hamn --headless vm create --profile work --cpu 4 --memory 4 --yes
hamn --headless vm start --profile work --yes
hamn --headless docker containers list --profile work
hamn --headless docker containers list --context remote
hamn --headless docker containers logs api --profile work --follow
hamn --headless k8s contexts list
hamn --headless k8s pods list --context dev --namespace default
hamn --headless k8s deployments scale api --replicas 3 --context dev --namespace default --yes
hamn --headless vm stop --profile work --yes
```

Mutations require `--yes` and explicit resource targets. One-shot commands emit
one JSON response; `--follow` logs and `--watch` queries emit NDJSON. stdout is
reserved for machine-readable responses. See [API](docs/API.md).

## Declarative configuration

Keep every setting of a profile in one file, and let `vm apply` make the
profile match it. A file `work.yaml`:

```yaml
apiVersion: hamn/v1
kind: Profile
metadata:
  name: work
spec:
  cpus: 6
  memoryMiB: 8192
  diskGiB: 80
  rosetta: true
  mounts:
    - location: "/Users/<your-user>/project"
      mountPoint: "/workspace/project"
      writable: true
```

```sh
hamn --headless vm apply --profile work --file work.yaml --dry-run
hamn --headless vm apply --profile work --file work.yaml --yes
hamn --headless vm start --profile work --yes
```

The first command reports what would change and writes nothing. The second
creates the profile when it does not exist and replaces its configuration when
the file differs. When nothing differs it writes nothing, so it can be run
again at any time, also while the VM runs.

- `metadata.name` must equal `--profile`.
- `spec` takes the same keys as the profile's `~/.hamn/<profile>/config.yaml`.
  The definition file is the whole configuration: a key that `spec` leaves out
  returns to its default. A disk cannot shrink, so keep `diskGiB` in the file:
  a value below the current size, also the default 60, is refused.
- `vm apply` changes the configuration only. It does not start or stop the VM,
  and a change needs a stopped VM: `vm stop`, `vm apply`, then `vm start`.
- `vm apply` does not check the Mac; the next `vm start` does. Replace the
  mount `location` with a directory that exists and that you own, inside your
  home directory for a writable mount, and keep `rosetta: true` only where
  Rosetta is available.

See [Declarative configuration](docs/CONFIGURATION.md#declarative-configuration)
for the rules, [YAML schema](docs/CONFIGURATION.md#yaml-schema) for every
setting, and [Mounts](docs/CONFIGURATION.md#mounts) for the keys and rules of a
mount.

## External tools

```sh
export DOCKER_HOST="unix://$HOME/.hamn/work/docker.sock"
docker compose up -d
docker buildx build --load -t example .
```

UI selection does not change Docker's current context or kubeconfig `current-context`.
Explicit `docker context use` and `kubectl config` commands retain their normal
write semantics. Headless authentication remains noninteractive; native CLI
authentication runs according to the installed CLI in the embedded terminal.

## Data

`vm delete` stops and hides a profile while preserving its disk and Docker data.
`system uninstall --yes` permanently removes all Hamn profiles and managed
installation files. [Configuration](docs/CONFIGURATION.md) describes persistence.

Container creation, Compose, exec, Kubernetes apply, and port-forward use the
installed native CLIs in the TUI. They are not added to the headless SDK operation
set. Hamn does not provide an MCP server.


## Upgrade

```sh
hamn upgrade           # install the latest release
hamn upgrade --check   # only report whether an update is available
```

```text
$ hamn upgrade
Checking for updates...
Updating Hamn 0.1.1 → 0.1.2...
Downloading Hamn 0.1.2  100%  4.4 MiB / 4.4 MiB
Downloading guest image  100%  1.1 GiB / 1.1 GiB  11.8 MiB/s
Installing...
Updated Hamn 0.1.1 → 0.1.2. Existing VMs were not restarted.

$ hamn upgrade
Checking for updates...
Hamn 0.1.2 is up to date.
```

Running VMs are not restarted and existing VM disks are not changed; the new guest
image is used for VMs created afterwards. An interrupted download resumes
automatically, or on the next `hamn upgrade`. Failures print one line with the
reason and what to do next. Automation can use `hamn upgrade --output json` or
`hamn --headless system upgrade --yes`. After a successful TUI session Hamn may
mention a newer release; it never installs one by itself. Set
`HAMN_NO_UPDATE_CHECK=1` to disable that check. See
[installation](docs/INSTALLATION.md) for integrity, recovery and `--force`.
