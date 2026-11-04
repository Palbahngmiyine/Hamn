# Hamn

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/branding/hamn-icon.svg">
  <img src="assets/branding/hamn-icon-on-light.svg" alt="Hamn" width="100">
</picture>

[한국어](README.ko.md)

----

Hamn manages a Linux VM, its Docker Engine, and external Kubernetes clusters
from one macOS executable. Run `hamn` for the terminal interface or
use `hamn --headless` for headless automation.

----

## Requirements

- Apple Silicon Mac with macOS 13 or later.
- Container images built for `linux/arm64`. The guest image has no amd64
  emulation (Rosetta or qemu), so a `linux/amd64` image does not run.

TUI container browsing requires Docker CLI; Kubernetes browsing requires kubectl.
Docker Desktop is not required.

## Install

```sh
curl -fsSL --proto '=https' --tlsv1.2 \
  https://github.com/Palbahngmiyine/Hamn/releases/latest/download/install.sh \
  | /bin/bash
```

It downloads Hamn and its Linux guest image, verifies both, and installs the
`hamn` command in `~/.local/bin`. Update later with `hamn upgrade`.

## Getting started

```sh
hamn
hamn --headless vm create --profile work --cpu 4 --memory 4 --yes
hamn --headless vm start --profile work --yes
export DOCKER_HOST="unix://$HOME/.hamn/work/docker.sock"
docker compose up -d
```

`hamn --headless capabilities` lists every headless operation.

## Development

Building Hamn from source needs Apple Silicon macOS 13+, Apple command-line
developer tools, and the Rust toolchain in `rust-toolchain.toml`.

## Security

Report vulnerabilities as described in [SECURITY.md](SECURITY.md).

## License

Hamn is licensed under the [MIT License](LICENSE).
