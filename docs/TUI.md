# Workspaces and native commands

Run `hamn` in a terminal. Choose **Containers** or **Kubernetes** on first launch;
subsequent launches open the saved workspace. Tab switches workspace and `,`
changes the saved default. Each workspace retains its target and browsing state.

Install Docker CLI for container browsing and kubectl for Kubernetes browsing.
Compose, buildx, kubectl plugins, and any exec credential helpers remain external
installations. Hamn still ships as one executable. Headless SDK operations retain
their existing contract and do not require these CLI installations.

## Navigation

| Key | Action |
| --- | --- |
| `:` | Enter a Docker or kubectl command |
| `/`, arrows or `j/k` | Filter visible rows and select a resource |
| Enter, `l`, `g` | Inspect, logs, statistics where supported |
| `s`, `t`, `r`, `d` | Resource start, stop, restart, delete where supported |
| `m` or `?` | Actions and command help |
| Tab, `,` | Switch workspace; change default workspace |
| `e` | Choose Hamn profile / external Docker context, or Kubernetes context |
| `n` | Choose Kubernetes namespace |
| `a` | Toggle all containers while preserving the current query filters |
| `v` | Open the selected Hamn environment's VM panel |
| `c` in VM panel | Edit CPU, memory (GiB), and disk (GiB) configuration command |
| `!` | Show the active lifecycle operation log |
| Esc, `q` | Return; quit |

Containers opens with running containers. A stopped Hamn environment offers
start; listing never boots a VM. The VM panel shows VM state separately from
Docker readiness. External Docker contexts have no Hamn VM controls.
The `a` key toggles Docker's `--all` value, including grouped flags such as `-as`
and repeated boolean options. It preserves filters, size, and `--last`/`--latest`;
those last two options retain Docker's behavior of including all container states.
Kubernetes opens Pods in the current valid context, otherwise the context picker.
Entering Kubernetes neither starts a VM nor migrates a Hamn profile.
Selecting contexts or namespaces does not write kubeconfig or Docker configuration.

## Commands

The `docker` / `kubectl` prefix is optional in its workspace. Full prefixes also
switch to the corresponding workspace. Examples:

```text
ps
ps -a --filter label=app=api
docker images
volume ls
network ls
pods
get pods -A
kubectl get deployments -n dev --sort-by=.metadata.name
```

Ordinary supported list queries use the installed CLI's JSON output to populate
selectable tables. CLI filtering, scope, and sorting run in the CLI.
`ps --size` (including `-as`) adds a Size column; repeated boolean options follow
the CLI's last-value rule. Grouped kubectl output/watch options such as
`get pods -Aoyaml` and `get pods -Aw` run unchanged in the embedded terminal.
Grouped connection options such as `docker -DHunix:///path/docker.sock ps` and
`get pods -Ashttps://api.example` also retain their explicit target in the header
and selected resource actions. The typed command's original arguments are preserved.
For `--all-namespaces` / `-A`, the last boolean value determines the scope.
A final `false` keeps the UI namespace default unless an explicit target overrides it.
Option values are not treated as flags: `--as-group -nteam` keeps the selected
namespace, just like `--as-group=-nteam`. A value that resembles `--context`,
`--kubeconfig`, or an output option does not change the UI defaults or list format.
Explicit output options such as `--format`, `-q`, and `-o yaml` are preserved and displayed
in the terminal. Other commands are passed to the installed CLI, including
Compose, buildx, `exec -it`, `attach`, `logs -f`, `stats`, `apply`, `edit`, and
`port-forward`. Compatibility aliases such as `containers` remain available.

Typed commands run once, with no additional Hamn confirmation or command deadline.
Changes selected through the action menu retain confirmation. Kubernetes menu
changes bind the selected UID and resource version: deletion sends server-side
preconditions through kubectl, and restart uses an atomic guarded patch. If the
resource changed, refresh and select it again. Changing a connection invalidates
the previous rows until the new target returns a list. Native `kubectl events`
(and `events`) keep their own command semantics; use `get events` for a table. Quotes and escaped
arguments are supported; shell pipes, redirection, variable expansion, and shell
aliases are not interpreted. Run a shell explicitly inside `exec` if required.
Structured query output is limited to 16 MiB; larger output reports an error.

UI selections supply connection defaults. Explicit Docker `--context` / `--host`
and kubectl `--context`, `--kubeconfig`, `--namespace` / `-n` take precedence;
`-A` retains its all-namespace scope. Place Docker global flags before its command,
as required by Docker CLI. The header shows the effective invocation target.
`docker context use` and `kubectl config` execute with their normal configuration
write semantics; Hamn reloads selection information after the terminal closes.
Selected-resource actions retain the query's TLS server name, certificates,
authentication, impersonation, and proxy overrides.

Installed kubectl plugins own their argument grammar. Hamn passes their original
arguments without injecting UI context/namespace flags, because kubectl rejects
flags before plugin names and plugins may not accept them. The header explicitly
shows **Plugin-defined target / inherited CLI configuration**. Specify a plugin's
connection options according to that plugin. This exception prevents accidental
argument rewriting; plugin support does not imply every plugin targets the UI selection.
An installed `kubectl-ns` or `kubectl-pods` takes precedence over the corresponding
Hamn convenience alias, with or without the `kubectl` prefix.
`kubectl create <extension>` plugins also retain their original arguments;
built-in create commands and their aliases keep precedence over plugin files.

The connection rules are based on the official [Docker CLI reference](https://docs.docker.com/reference/cli/docker/),
[kubectl reference](https://kubernetes.io/docs/reference/kubectl/), and
[kubectl plugin contract](https://kubernetes.io/docs/tasks/extend-kubectl/kubectl-plugins/).

## Embedded terminal and lifecycle operations

The embedded PTY supplies terminal input/output and resizes with the window.
Ctrl-C goes to the CLI; Docker's default Ctrl-P Ctrl-Q detach sequence is passed
through. After the command exits, its exit code remains visible. Enter or Esc
returns to the previous browser and refreshes its resources. Shift+PageUp/PageDown
scrolls terminal history while a command runs; PageUp/PageDown also scrolls after
exit. Normal input returns to the live output. Input is queued in order up to 4 MiB;
a paste or key that would exceed this limit is rejected with a visible message.
Ctrl-C retains the CLI's raw-byte behavior. Ctrl+Alt+C explicitly discards queued
and terminal-buffered input and sends SIGINT to the CLI process group; the discarded
queue byte count is shown. Undelivered input on CLI exit or write failure is reported. In the `!` operation log, arrows
or `j/k` scroll the log.

VM start/stop/recovery runs independently of list queries. Navigation, refresh,
and ordinary Esc do not cancel it. Quitting during a lifecycle mutation asks to
cancel and exit, then waits for child termination and cleanup. A cancelled start
only stops a VM it created after remote cleanup is confirmed. If completion cannot
be established, it preserves the VM and reports recovery required. The operation
log remains available while it runs, retaining the latest 1 MiB. A busy renderer
applies backpressure instead of silently dropping queued worker logs. After the
worker exits, Hamn drains the queued bytes without waiting for background stderr
holders; renderer delays cannot expire that drain. If a worker
exits without a result, its error includes the last 8 KiB of diagnostics.
An unsuccessful SSH mutation also fences its dispatch token before waiting for
remote completion. If settlement cannot be verified, further guest mutations in
that operation are blocked and the VM is preserved for recovery.
External changes are not described as rolled back merely because their CLI exited.

A successful VM stop can include an earlier retirement warning. The operation
status and log retain that warning; an `outcomeUnknown` warning also retains the
original profile and `vm migrate` diagnostic in `!`. Known failures remain warnings
without being relabeled unknown. These results stay in the Containers workspace
when another workspace is visible and are printed after terminal restoration on exit.
Long diagnostics use a one-line header summary with `! log`; the complete text
remains in the scrollable operation detail without hiding the resource list.

A running VM is not proof of Docker availability. Readiness distinguishes ready,
preparing, unavailable, and recovery required; successful start requires the host
socket and a real Docker `/_ping` response. An interrupted operation retains its
identity and outcome for inspection on the next launch. Rejected preflight input
with no VM changes is a known failure; it does not create a new recovery alarm. See [API](API.md).

Completed K3s retirement and unfinished Docker deployment are checked separately.
A complete, owned backup with matching helper contract and retirement provenance
can be rolled back and retried. Legacy backups require trusted helper identities
and full metadata validation. Ambiguous, partial, or altered backups are preserved
with an error. Recovery never deletes Docker containers, images, or volumes.

## Preferences

`~/.hamn/tui.json` stores `{"version":1,"defaultWorkspace":"containers"}` or
`"kubernetes"`. Writes use a private temporary file, file synchronization, atomic
rename, directory synchronization, and mode `0600`. Invalid versions, malformed
JSON, unsafe permissions, and symlink reads show an error and return to selection.
Choosing again writes a valid preference file. Workspace connection selections
are session-local; this file only persists the default workspace.
