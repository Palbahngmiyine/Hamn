# Hamn

Hamn manages a Linux VM, its Docker Engine, and external Kubernetes clusters
from one macOS executable. Run `hamn` for the Ratatui terminal interface or
use `hamn --headless` for JSON and NDJSON automation.

## Requirements

- Apple Silicon Mac with macOS 13 or later.
- A signed Hamn guest image for VM and Docker operations.
- A kubeconfig for Kubernetes operations. These work independently of the VM.

The built-in Docker client does not require Docker CLI or Docker Desktop.
External Docker CLI, Compose, buildx, and SDKs can use the profile socket.

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

Use `:vm`, `:containers`, `:images`, `:volumes`, `:networks`, `:contexts`, `:ns`,
and `:pods` to select a resource view. `/` filters, arrows or `j/k` select,
Enter opens details, Esc returns, and `?` shows help. The selected profile,
context and namespace appear in the header. Changes require confirmation.
Closing the interface leaves running VMs running.

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

Hamn does not change Docker's current context or kubeconfig `current-context`.
Kubernetes supports `--kubeconfig`, `KUBECONFIG`, or `~/.kube/config` and uses only
the explicitly selected context. Interactive authentication must be completed
outside Hamn before using that context.

## Migration and data

This revision replaces the old CLI and JSON format. Managed K3s is removed.
The first TUI session retires running legacy profiles; stopped profiles retire
on their next start. VM/Docker mutations also run the retirement preflight.
Read-only commands report pending migration without running it.

**K3s cluster data and its dedicated local volumes are permanently deleted.**
Rolling back the Hamn executable cannot recover them. Retirement preserves
Docker's `moby` namespace, Docker volumes, shared containerd content storage,
user mounts, and the original kubeconfig. Interrupted retirement resumes from
its durable journal and does not mark a failed migration complete.

`vm delete` stops and hides a profile while preserving its disk and Docker data.
`system uninstall --yes` permanently removes all Hamn profiles and managed
installation files. [Configuration](docs/CONFIGURATION.md) describes persistence.

Container creation, Compose execution, arbitrary shell/exec, Kubernetes apply,
port-forward, and an MCP server are outside the built-in command set.
