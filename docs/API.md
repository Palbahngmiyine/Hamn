# Control API

The public interface is `hamn --headless <operation> [arguments]`. TUI VM controls
call the same service; native Docker/kubectl commands use the installed CLI
through a PTY and are not additional headless operations.
`hamn --headless capabilities` lists available operations and mutation flags.

## Response contract

```json
{"schemaVersion":1,"requestId":"example","ok":true,"target":{"profile":"work","context":null,"namespace":null,"name":null},"data":[],"error":null}
```

Errors have `ok:false`, `data:null`, and `error:{"code":"...","message":"..."}`.
Treat error codes as machine-readable; messages are diagnostic text. A nonzero
process exit means failure. Parse errors and missing-terminal errors also use
JSON. `--help` and `--version` print text.

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
`state:running` describes the VM process only; `ready` requires Docker `/_ping`.
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

`vm stop` may succeed after its preliminary retirement failed. In that case,
`data.migrationError` retains the earlier `{code,message}` while the stop response
remains successful. Inspect this field separately from the stop result and the
latest operation record, which may now describe the completed stop.

## Operations

| Family | Operations |
| --- | --- |
| VM | `vm list`, `status`, `create`, `configure`, `start`, `stop`, `delete`, `migrate`, `diagnostics`, `env` |
| Docker containers | `docker containers list`, `inspect`, `logs`, `stats`, `start`, `stop`, `restart`, `delete` |
| Docker inventory | `docker images list`, `docker volumes list`, `docker networks list` |
| Kubernetes selection | `k8s contexts list`, `k8s namespaces list` |
| Kubernetes resources | `k8s <resource> list`; pods, deployments, statefulsets, daemonsets, services, nodes, events, jobs, cronjobs, ingresses, pvcs |
| Kubernetes details | `k8s <resource> inspect <name>` for every listed resource and namespaces; JSON object and YAML |
| Kubernetes logs | `k8s pods logs` |
| Kubernetes mutations | `k8s deployments scale/restart`, `statefulsets scale/restart`, `daemonsets restart`, `pods delete` |
| Maintenance | `system update`, `system uninstall` |

VM and Docker requests require `--profile`, except `vm list`. VM create and
configure accept `--cpu`, `--memory` (GiB), and `--disk` (GiB). `vm diagnostics`
accepts `--path` for an archive. `system update` accepts `--manifest`.
`vm env` returns Docker connection information, not shell text.

Kubernetes requests require `--context`, except context listing. Namespaced
mutations additionally require `--namespace`. Lists support `--all-namespaces`.
A resource name can follow the operation or use `--name`. `--uid` prevents
operating on a replacement Kubernetes object. Scale requires `--replicas`;
zero is allowed. Pod logs accept `--container` and `--previous`.

All headless mutations require `--yes`; TUI VM confirmation supplies it after approval.
Typed native CLI commands retain their own confirmation and output semantics.
Mutations cannot use `--watch`, `--follow`, or `--all-namespaces`.

## Connections and ownership

Docker API requests go directly to `~/.hamn/<profile>/docker.sock`, after Engine
API version negotiation. Container names resolve to immutable IDs before
mutation. Removal preserves volumes. The C port observer still owns forwarded
Docker-published ports. External Docker tools may connect to this same socket.

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
