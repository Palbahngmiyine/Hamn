# Colima coexistence and migration

See [COLIMA-COMPATIBILITY.ko.md](COLIMA-COMPATIBILITY.ko.md) for Korean.
Hamn manages its own Apple Silicon VM and external kubeconfig clusters. It
does not reuse Colima profiles, disks, sockets, or configuration.

## Operations

Run `hamn` for the TUI. Automated operations use the following interface:

| Intent | Hamn command |
| --- | --- |
| Start a profile | `hamn --headless vm start --profile work --yes` |
| Stop a profile | `hamn --headless vm stop --profile work --yes` |
| Remove a profile from active listings, retain its disk | `hamn --headless vm delete --profile work --yes` |
| List profiles | `hamn --headless vm list` |
| Inspect status | `hamn --headless vm status --profile work` |
| Configure a stopped profile | `hamn --headless vm configure --profile work --cpu 6 --memory 8 --disk 80 --yes` |
| Read Docker connection information | `hamn --headless vm env --profile work` |
| List containers | `hamn --headless docker containers list --profile work` |
| List external Kubernetes contexts | `hamn --headless k8s contexts list` |
| List external Pods | `hamn --headless k8s pods list --context dev --namespace default` |
| Remove Hamn and all managed data | `hamn --headless system uninstall --yes` |

Memory and disk arguments are GiB. Results are JSON, including `vm env`;
do not pass its output to `eval`. The old CLI and JSON formats are not
compatible. There is no managed K3s start command, arbitrary guest shell,
container creation, Compose execution, Kubernetes apply, or exec operation
in the built-in interface. External tools can continue using Docker Engine API.

## External Docker tools

Hamn does not require Docker CLI to boot a VM or manage containers. It does
not switch `docker context` or restore a previous context when stopping.
Select the socket explicitly for external tools:

```sh
hamn --headless vm start --profile work --yes
DOCKER_HOST="unix://$HOME/.hamn/work/docker.sock" docker ps
DOCKER_HOST="unix://$HOME/.hamn/work/docker.sock" docker compose up -d
DOCKER_HOST="unix://$HOME/.hamn/work/docker.sock" docker buildx build --load -t example .
```

The public endpoint is the profile socket, not `/var/run/docker.sock`.
Docker uses the guest containerd `moby` namespace. Guest containerd and CRI
sockets are not public host APIs. Network mode remains shared NAT with
owned TCP/UDP forwarding. See [Architecture](ARCHITECTURE.md).

## Migration boundaries

Leave Colima's VM, Docker context, socket, installation, and state as they
were. Install Hamn separately, choose a named Hamn profile, and direct only
the intended Docker commands or SDK to its socket. Verify your application's
Compose/buildx/SDK behavior, then stop Hamn and check Colima is unchanged.
Docker objects are not copied from Colima automatically.

Upgrading an old Hamn installation is different: managed K3s is retired
automatically. Its cluster data and dedicated local volumes are permanently
deleted; Docker objects, named volumes, and user mounts are preserved.
Original kubeconfig files are preserved, while old owned local contexts are
reported unavailable. Binary rollback cannot restore K3s data. See
[Configuration](CONFIGURATION.md) for migration timing and failure handling.
