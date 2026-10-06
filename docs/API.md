# Control API

The public interface is `hamn --headless <operation> [arguments]`. TUI VM controls
call the same service; native Docker/kubectl commands use the installed CLI
through a PTY and are not additional headless operations.
`hamn --headless capabilities` lists available operations and mutation flags.

## Response contract

```json
{"schemaVersion":1,"requestId":"example","ok":true,"target":{"profile":"work","context":null,"namespace":null,"name":null,"dockerConfig":null},"data":[],"error":null}
```

Errors have `ok:false`, `data:null`, and `error:{"code":"...","message":"..."}`.
Treat error codes as machine-readable; messages are diagnostic text. A nonzero
process exit means failure. Parse errors and missing-terminal errors also use
JSON. `--help` and `--version` print text.

Docker operations separate a daemon that failed from one that was not reached.
`engineError` is a 5xx answer from the Engine to a read or to the name
resolution before a mutation, for example a storage error: the daemon is
running and nothing was changed. `dockerUnavailable` means no Engine answer was
received. A 5xx answer to the mutation request itself is `outcomeUnknown`.

`--watch` repeats queries every two seconds. `--follow` streams Docker container
or Pod logs. Log queries use NDJSON even without `--follow`. Each NDJSON record
carries the response fields plus `type` and
`sequence`; log records precede the final result. Streams apply bounded
backpressure. Log lines and one-shot log responses are limited to 1 MiB;
`--tail` accepts 0 through 10000. The default tail is 200.

`--timeout` is a deadline in seconds (default 600, allowed 1–3600). Ctrl-C and
SIGTERM request cancellation. An external mutation cancelled after dispatch can return
`outcomeUnknown`: a server may already have applied it. Managed VM cancellation
waits for cleanup and reports `cancelled` only when recovery is confirmed. Read the target's state
before retrying. Hamn never claims to undo an accepted server mutation.

## VM readiness and operation history

VM status retains existing fields and adds `dockerStatus` (`ready`, `preparing`,
`unavailable`, `recoveryRequired`) and `lastOperation` (null when absent).
`mountHome`, `homeReadOnly`, `mountInotify`, `rosetta` and `fileEvents` expose
configured sharing/translation settings; `fileEvents` is `disabled` or
`best-effort-existing-files`, not a full hot-reload guarantee.
`sharedDirectories` lists the host directories a container can bind-mount, as
configured: `hostPath`, the `guestPath` that Docker clients must name, and
`writable`. The home share comes first when `mountHome` is true, then each
`mounts` entry. A bind mount of any other host path sees only the guest. A
running VM keeps the shares it was started with.
`portForwardFailures` lists the published ports that the running VM's port
observer could not forward at its last pass: `hostIp`, `hostPort`, `protocol`
and `reason`, which is `hostPortInUse` when another host process holds the port
and `forwardFailed` otherwise. Docker reports such a container as started and
shows the mapping; the port is forwarded once the cause is gone. The array is
empty for a VM that is not running.
`state:running` describes the VM process only; `ready` requires Docker `/_ping`.
`hostFreeMiB` is the space available on the volume that holds the profile
directory, where the sparse VM disk grows; it is null when it cannot be
measured. Below 10 GiB, `vm start` prints a warning to stderr and continues: a
full volume makes the guest's Docker store fail with I/O errors.
`lastOperation` includes `schemaVersion`, `operationId`, `operation`, `status`,
`phase`, `startedVm`, `exitCode`, and `error`. While running, exit/error may be absent.
Ownership evidence includes PID, process start time, and executable UUID; PID alone
never establishes ownership. Completed, failed, cancelled, and unknown outcomes
remain in the private atomic `~/.hamn/<profile>/operation.json` record.
An orphaned running record is reported as `outcomeUnknown` / `recoveryRequired`.
After preparing a missing signed image, `restartRequired` with phase
`signed-image-ready` records the handoff to the installed binary. Both frontends
retry start once; this handoff does not claim VM readiness or clear an earlier
`recoveryRequired` outcome. A later successful start establishes completion.
Navigation does not cancel managed work; confirmed quit requests cancellation
and waits for cleanup. Unknown outcomes require inspection before retry.

## Operations

| Family | Operations |
| --- | --- |
| VM | `vm list`, `status`, `create`, `configure`, `start`, `stop`, `delete`, `diagnostics`, `env` |
| Docker containers | `docker containers list`, `inspect`, `logs`, `stats`, `start`, `stop`, `restart`, `delete` |
| Docker inventory | `docker images list`, `docker volumes list`, `docker networks list` |
| Kubernetes selection | `k8s contexts list`, `k8s namespaces list` |
| Kubernetes resources | `k8s <resource> list`; pods, deployments, statefulsets, daemonsets, services, nodes, events, jobs, cronjobs, ingresses, pvcs |
| Kubernetes details | `k8s <resource> inspect <name>` for every listed resource and namespaces; JSON object and YAML |
| Kubernetes logs | `k8s pods logs` |
| Kubernetes mutations | `k8s deployments scale/restart`, `statefulsets scale/restart`, `daemonsets restart`, `pods delete` |
| Maintenance | `system upgrade`, `system uninstall` |

VM requests require `--profile`, except `vm list`. Docker requests require exactly
one of `--profile` or `--context`. External contexts use Docker CLI authentication
and transport; `--docker-config` optionally selects its configuration directory.
They do not query or mutate Hamn profiles. VM create and
configure accept `--cpu`, `--memory` (GiB), `--disk` (GiB), and
`--rosetta <true|false>`; `vm start` accepts the first three. An omitted
argument keeps the profile's value. `vm diagnostics`
accepts `--path` for an archive: a ustar file holding `manifest.json`,
`status.json` (the VM state, `dockerStatus`, `hostFreeMiB` and
`portForwardFailures` that `vm status` reports), `operation.json` (the last operation record, or `null`), and redacted
tails of `logs/serial.log`, `logs/serial.previous.log`, `logs/vmrun.log` and
`logs/port-observer.log`.
`system upgrade` accepts
`--manifest`, `--check` (read-only, no `--yes` required), and `--force`
(same-version reinstall, still requires `--yes`). `--check` conflicts with `--force`.
Use `hamn --headless system upgrade --help` for upgrade-specific usage and recovery.
See [installation and upgrade experience](INSTALLATION.md) for progress and compatibility.
`vm env` returns Docker connection information, not shell text.

Kubernetes requests require `--context`, except context listing. Namespaced
mutations additionally require `--namespace`. Lists support `--all-namespaces`.
A resource name can follow the operation or use `--name`. `--uid` prevents
operating on a replacement Kubernetes object. Scale requires `--replicas`;
zero is allowed. Pod logs accept `--container` and `--previous`.

`docker containers start`, `stop` and `restart` return the state they read back
after the Engine accepted the request, under the Engine's key names:
`{"Id":"...","Name":"/...","State":{...}}`. `State` holds `Status`, `Running`,
`Paused`, `Restarting`, `OOMKilled`, `Dead`, `ExitCode`, `Error`, `StartedAt`
and `FinishedAt`, each when the Engine reports it. The rest of the inspection,
such as `Config.Env`, is not returned; use `docker containers inspect` for the
full Engine object. If the state cannot be read back the result is
`outcomeUnknown`. `docker containers delete` returns `{"deleted":true,"id":"..."}`.

All headless mutations require `--yes`; TUI VM confirmation supplies it after approval.
Typed native CLI commands retain their own confirmation and output semantics.
Mutations cannot use `--watch`, `--follow`, or `--all-namespaces`.

## Connections and ownership

Profile Docker API requests go directly to `~/.hamn/<profile>/docker.sock`, after Engine
API version negotiation. Container names resolve to immutable IDs before
mutation. Container removal preserves the container's named and anonymous
volumes; remove unused ones with the Docker CLI. The C port observer still owns
forwarded Docker-published ports. External Docker tools may connect to this same
socket.
For `--context`, a private temporary socket proxies the existing Engine API client
through `docker --context <name> system dial-stdio`; Docker owns TLS/SSH/context
configuration. The response schema, ID resolution and mutation error contract stay
the same. Deadline/cancellation kills and reaps only owned CLI groups and removes
the temporary socket. An unavailable context never falls back to a profile or default.
When the Docker CLI transport exits unsuccessfully, the error message carries its
exit status and stderr, such as an unknown context name. The code is
`dockerUnavailable`; a mutation reports `outcomeUnknown` only if the transport had
already relayed an Engine response, because before that no mutation was sent.

Kubernetes loads an explicit `--kubeconfig`, otherwise `KUBECONFIG`, otherwise
`~/.kube/config`. Context and namespace selection never write those files.
Certificates and tokens are handled by kube-rs. Hamn resolves exec credentials
asynchronously before constructing the client. Interactive authentication is
rejected. Exec credentials run without terminal input, with bounded output and operation
deadlines. Hamn resolves them again for each operation or watch snapshot.
Legacy `auth-provider` configurations must be replaced with exec credentials.

C VM operations run inside a fresh process of the same executable. The
`host/core/control.h` ABI borrows input strings. Returned UTF-8 JSON belongs to
the caller and must be freed with `hamn_control_free`; it is generated from C
state, not parsed from legacy command output. C stdout is routed away from the
worker protocol. Process identity checks and lifecycle locks remain in C.

`__core-worker`, `vmrun`, forwarding process modes, and guest `hamnd` endpoints
are private implementation details, not public automation interfaces. There is
no public containerd socket, built-in Compose/exec/apply/port-forward, or MCP
server in this interface.

See [Cargo static linking](https://doc.rust-lang.org/cargo/reference/build-script-examples.html#building-a-native-library),
[Ratatui backends](https://ratatui.rs/concepts/backends/), and
[kubeconfig rules](https://kubernetes.io/docs/concepts/configuration/organize-cluster-access-kubeconfig/)
for the upstream contracts used by the implementation.
