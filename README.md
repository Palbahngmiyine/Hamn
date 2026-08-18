# Hamn

Hamn runs Docker workloads in a Linux VM on Apple Silicon Macs. Use your usual
Docker CLI, Compose, buildx, SDKs, Testcontainers, and optional K3s with
isolated profiles.

## Requirements

- Apple Silicon Mac running macOS 13 or later
- A Docker CLI installed separately. Docker Desktop is not required.

## Install

```sh
curl -fsSL --proto '=https' --tlsv1.2 \
  https://github.com/Palbahngmiyine/Hamn/releases/latest/download/install.sh \
  | /bin/bash
```

For a bootstrap verified before execution, follow the attestation procedure in
[Public release repository setup](docs/RELEASE-SETUP.md#6-install).

Ensure `~/.local/bin` is on your `PATH`, then start Hamn and use Docker:

```sh
hamn start
docker run --rm alpine uname -m
docker compose up -d
docker ps
```

Hamn creates the Docker context for the active profile automatically. After it
starts, use Docker commands exactly as you normally would.

## Everyday commands

```sh
hamn status
hamn stop
hamn start

docker compose down --volumes
docker buildx build --load -t example .
```

## Profiles

Profiles keep their VM, Docker socket, configuration, and data separate.

```sh
hamn start --profile work
hamn configure --profile work --cpu 6 --memory 8 --disk 80
hamn status --profile work

# Docker SDKs and Testcontainers
eval "$(hamn env --profile work)"
```

Use `hamn template` to view the default configuration or `hamn start --edit`
to edit it before startup.

## Kubernetes

K3s is off by default. Enable it for the profile you want to use:

```sh
hamn start --profile dev
hamn kubernetes start --profile dev
hamn kubectl --profile dev -- get nodes
```

## Files, ports, and architecture emulation

- Your home directory is mounted into the VM by default. Configure additional
  mounts with `hamn configure`.
- Docker-published TCP and UDP ports are available from macOS. Containers can
  reach the Mac at `host.docker.internal`.
- amd64 containers work through `binfmt`; Rosetta is available as an opt-in
  profile setting when supported by macOS.

## Remove a profile

`hamn delete` stops a profile and keeps its data for later reuse.
`hamn delete --data` permanently removes that profile's data after confirmation.

## Learn more

- [Configuration](docs/CONFIGURATION.md)
- [Colima migration and command mapping](docs/COLIMA-COMPATIBILITY.md)
- [Security policy](SECURITY.md)
