# Hamn

Hamn manages a Linux VM, its Docker Engine, and external Kubernetes clusters
from one macOS executable. Run `hamn` for the Ratatui terminal interface or
use `hamn --headless` for JSON and NDJSON automation.

## Requirements

- Apple Silicon Mac with macOS 13 or later.
- A signed Hamn guest image for VM and Docker operations.
- A kubeconfig for Kubernetes operations. These work independently of the VM.

TUI container browsing requires Docker CLI; Kubernetes browsing requires kubectl.
Headless SDK operations do not require these CLIs. Compose, buildx, and plugins
remain external installations. Docker Desktop is not required.

## Install

```sh
curl -fsSL --proto '=https' --tlsv1.2 \
  https://github.com/Palbahngmiyine/Hamn/releases/latest/download/install.sh \
  | /bin/bash
```

```sh
hamn
```

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
hamn --headless docker containers logs api --profile work --follow
hamn --headless k8s contexts list
hamn --headless k8s pods list --context dev --namespace default
hamn --headless k8s deployments scale api --replicas 3 --context dev --namespace default --yes
hamn --headless vm stop --profile work --yes
```

Mutations require `--yes` and explicit resource targets. One-shot commands emit
one JSON response; `--follow` logs and `--watch` queries emit NDJSON. stdout is
reserved for machine-readable responses. See [API](docs/API.md).

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

## Migration and data

This revision replaces the old CLI and JSON format. Managed K3s is removed.
TUI entry performs no retirement. Legacy profiles retire on their next VM/Docker
mutation; stopped profiles can retire during their next start.
Read-only commands report pending migration without running it.

**K3s cluster data and its dedicated local volumes are permanently deleted.**
Rolling back the Hamn executable cannot recover them. Retirement preserves
Docker's `moby` namespace, Docker volumes, shared containerd content storage,
user mounts, and the original kubeconfig. Interrupted retirement resumes from
its durable journal and does not mark a failed migration complete.

`vm delete` stops and hides a profile while preserving its disk and Docker data.
`system uninstall --yes` permanently removes all Hamn profiles and managed
installation files. [Configuration](docs/CONFIGURATION.md) describes persistence.

Container creation, Compose, exec, Kubernetes apply, and port-forward use the
installed native CLIs in the TUI. They are not added to the headless SDK operation
set. Hamn does not provide an MCP server.
