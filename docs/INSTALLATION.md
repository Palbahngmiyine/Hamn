# Installation and upgrade

See [INSTALLATION.ko.md](INSTALLATION.ko.md) for Korean.

The published installer (`install.sh`) uses macOS system commands, including
the built-in Bash and zsh shells, only until it has authenticated the release's
`hamn` executable. No Python, Perl, Ruby, Homebrew, Rust, or Xcode Command Line
Tools installation is needed on the macOS host. The bootstrap verifies the
archive's pinned SHA-256 with the system `openssl`, reads only its executable
through system `tar` into a private file, then uses that authenticated executable
to validate the complete archive before extracting it. The verified executable
then installs itself: manifest validation, receipt compatibility, transaction and
install locks, the recovery journal, generation publication and rollback, and
generation collection run inside Hamn (the private `hamn __install-support` mode);
no shell script installs or updates Hamn. Before that executable is available,
stock `zsh/system` owns host-download locking and bounded partial writes.
`make install` runs the same native installer from the built executable.
Developer builds, release assembly and tests still require their documented
toolchain.

Use the [official installer](../README.md#install) for a first installation or
an incompatible older installation (see below). For a managed installation:

```sh
hamn upgrade --check
hamn upgrade
hamn upgrade --force --output json
hamn --headless system upgrade --yes
hamn --version
```

Explicit human `upgrade` authorizes the
installation; headless mutations continue to require `--yes`. `--check` fetches
only the manifest and reports `up-to-date`, `repair-required`, `update-available`,
or `ahead` without creating directories, acquiring payloads, recovering a journal,
or changing installation state. Unsupported installations return `unsupported-install`
without network access. `--check` and `--force` conflict. Mutations reject
stable downgrades, development versions, source builds, external package-manager
installations and direct generation binaries; invoke the managed command symlink.
`--manifest URL` selects another HTTPS manifest. Local paths remain test-only.

Human output is short. Progress goes to stderr: `Checking for updates...`, an
`Updating Hamn A → B...` heading, one download line per artifact that is not
already cached (redrawn in place with percent, size and rate when the final
stderr is a terminal; a single start line otherwise), `Installing...`, and one
result line such as `Hamn A is up to date.` or `Updated Hamn A → B. Existing VMs
were not restarted.`. `--check` prints one sentence naming the next command.
A failure prints one `hamn upgrade: <reason>` line; headless reports the same
reason once, in its JSON `error.message`. `--output json` writes one
result object with `schemaVersion`, `currentVersion`, `latestVersion`, `status`,
`downloadedBytes`, `resumedBytes`, `reusedBytes`, per-artifact `artifacts`,
`completed`, and `profileDisksChanged=false`. Headless retains its existing JSON
envelope around this data. Stage messages use stderr. `resumedBytes` describes
the subset of downloaded bytes delivered by a valid Range response; it must not
be added to `downloadedBytes` when calculating total network traffic.

Before changing a generation, the updater validates the manifest, platform,
exact artifact size, SHA-256 and extracted host version. HTTPS is required for
initial URLs and every redirect. Runtime digest checks do not verify release
pipeline keyless attestations and must not be described as client signature
verification. Only schema v3 manifests are read; a manifest of another schema,
including the retired schema v2, fails with `manifest schema vN is not supported`
and the advice to reinstall with the official installer.

The generation receipt (schema 2) binds the version and host/guest digests to the
installed generation's `bin` and `share` trees: the executable and its manifest
pointer. A receipt of another schema is never reused. A healthy matching
receipt and selected image produce a no-op with zero payload requests. A damaged
guest selection or image is repaired without replacing a healthy host. `--force` permits same-version
host reinstall while still reusing verified artifacts. Host integrity damage
requires verified host reinstallation. No version string alone authorizes reuse.

Downloads live under `~/.hamn/cache/downloads/`, indexed by SHA-256. Owner-only
locks serialize each digest. Safe partial files resume with Range and a recorded
validator when available; a rejected or ignored Range gets one clean retry.
Size or digest mismatch is never published. The standalone installer's stock-shell
host bootstrap shares the digest lock and verified artifact cache with Hamn's
native downloader. Once the host is authenticated, its native updater acquires
the guest image. No telemetry is sent.
Explicit transfers allow 15 seconds to connect and fail only when throughput stays
below 1 KiB/s for 60 seconds (with an absolute six-hour bound), so a slow but
healthy link can finish a large guest image. When a transfer is interrupted
after persisting new bytes, the updater resumes it with Range up to three more
times; a transfer that makes no progress fails immediately, and integrity
failures never retry. Otherwise rerun the command to resume the safe partial.
`hamn upgrade` allows the operation 60 minutes; headless `system upgrade` keeps
its `--timeout` (default 600 seconds).

Managed installs collect obsolete generations after commit, retaining the active
and immediately previous generation, open executables, and recovery
references. Install and update transactions serialize on both target roots. A
pending recovery journal, failed process scan, or uncertain ownership defers
collection; retrying installation/update retries cleanup. Interrupted retirement
is also retried. Unmarked directories, incomplete staging copies, external package
manager files, profiles and guest images are outside collection. Do not manually
launch inactive generation paths during collection. Updaters from Hamn 0.1.1 and
earlier do not take the transaction locks and are not detected; do not run one
while installing or upgrading. Generations of the earlier layout (below) are
never collected.


A successful interactive TUI exit may display cached update information and
schedule a detached manifest check. No network is awaited by the TUI, and checks
never run inside an active CLI session or headless/internal commands. Successful
checks have a 24-hour TTL, failed checks a 6-hour backoff, and notices for a version
appear at most once per 24 hours. Checks require stdout/stderr TTYs and a managed
stable installation. `CI` or `HAMN_NO_UPDATE_CHECK=1` disables the automatic path.
An automatic request has a 2-second connect and 5-second total deadline. Cache
files are `~/.hamn/cache/update-check-v1.json` and `update-notice-v1.json`.

The updater serializes recovery and publication and uses a durable journal
(version 3), which records the exact attempted generation before the installer
publishes its command link. Recovery changes state only when the active target
is the recorded prior or attempted generation; a later installation from another
HOME is preserved. Selection-only repair never rewrites the binary pointer.
Journals written by Hamn 0.1.2 and earlier (v1) or by pre-release builds (v2)
record no attempted generation and are refused, pending or retired, with a
message naming the journal: check the command link and guest image selection
against the journal's `old-target` and `previous-selection`, then move the journal
aside and run the command again. If a journal cannot prove ownership of a changed
target, recovery fails without changing either selection or retiring the journal;
explicit inspection of the journal and generation history is required; retrying
alone cannot resolve this ambiguity. Do not delete that
evidence or force a binary rollback to bypass the check. Retry an interrupted
mutation from its original HOME with the same options, including `--manifest`. Recovery can
itself fail and leaves a pending journal that blocks unsafe VM startup. Updates
do not restart VMs or replace existing profile disks. Binary rollback cannot
restore guest state or retired legacy K3s data. First-install failure has no prior
binary to restore; a published command may remain while image selection is restored.

Bootstrap runs under `/bin/bash` with a fixed system-tool PATH
(`/usr/bin:/bin:/usr/sbin:/sbin`), and the native installer and updater run only
absolute system tools (`/usr/bin/curl`, `/usr/sbin/lsof`, `/usr/bin/sw_vers`), so
GNU tools earlier in a caller's PATH cannot change their behavior. Bootstrap
preserves the caller's PATH only for setup advice, which names
the line to add for the caller's zsh, bash or fish. It identifies an earlier conflicting `hamn`, and recommends opening
a new terminal after PATH changes. Pre-generation installs are not migrated: a
standalone `hamn` executable, a `.hamn-binary.sha256` marker or an empty
`.hamn-managed` data marker is refused, unchanged and never executed, with a message
naming what to move aside before installing again. A local install pass is not
physical VM validation.

## Installed layout and earlier installations

A managed installation is the command link `~/.local/bin/hamn` to `bin/hamn` in
one immutable generation directory,
`~/.local/share/hamn/src/.hamn-generations/<sha256>-<suffix>/`. A release
archive is exactly one generation payload: `bin/hamn` and
`share/hamn/update-manifest-url`, the manifest that `hamn upgrade` reads. The
installer adds an owner-only marker (layout version 2) and, when it replaces a
generation, the target it replaced. Generations carry no scripts or source
files; the data directory keeps its earlier name, `share/hamn/src`, so the
default paths of existing setups remain valid.

Installations made by Hamn 0.1.2 and earlier, and by pre-release builds, use the
earlier layout (layout version 1), whose generations carried the updater's shell
scripts. The current installer never adopts or changes such a generation:
`install.sh` and `make install` refuse it, unchanged, with a message naming the
command link and data directory to move aside. Move both aside, then run the
official installer again:

```sh
mv ~/.local/bin/hamn ~/.local/bin/hamn.earlier
mv ~/.local/share/hamn/src ~/.local/share/hamn/src.earlier
```

Profiles, VM disks and the guest image cache under `~/.hamn` are kept. Remove
the moved paths once the new installation works. Hamn 0.1.2 and earlier cannot
reach a new release in place (below); a pre-release build that reads schema v3
fails its own `hamn upgrade` at host artifact validation, because new archives
contain no installer scripts. Both are reinstalled the same way.

## Manifest compatibility and release evidence

The publisher writes schema v3 `hamn-update-manifest-v3.json`, which installations
point to. V3 names exact artifact sizes, the qcow2/zlib guest format and an 8 GiB
virtual size. Manifests are limited to 256 KiB, host archives to 128 MiB, and guest
artifacts to less than 2 GiB. Duplicate, unknown and null JSON keys are rejected,
including the retired v2 `repository` extension. Hamn 0.1.2 and earlier read only
schema v2, so they cannot upgrade in place; reinstall them with the official
installer as described above. Published
historical releases are not rewritten.

Publication requires actual image size evidence and a reviewed
`guest/image/release-size-budget.json`. A review-only report or absent reviewed
budget blocks publication. Hosted validation does not claim physical VM behavior.

## Reference analysis

References were limited to Microsoft, GitHub, the Rust Foundation ecosystem, and
Anthropic's Claude Code. These are interaction choices, not measured usability rankings.

| Official project | Observed pattern | Hamn adaptation |
| --- | --- | --- |
| [GitHub CLI](https://cli.github.com/manual/gh_extension_upgrade) | Operation-specific usage, flags, and dry-run explanation; [update notices use stderr](https://cli.github.com/manual/gh_help_environment). | Update-specific help, explicit `--yes`, progress separate from JSON. |
| [Microsoft .NET installer](https://learn.microsoft.com/en-us/dotnet/core/tools/dotnet-install-script) | [Source](https://github.com/dotnet/install-scripts/blob/47940ac9fc30a2f2dd19167165d0bb0774625f67/src/dotnet-install.sh) reports download, extraction, installed version, PATH advice, and a repeatable dry-run invocation. | Stage messages, actionable retry guidance, conditional PATH advice. |
| [Claude Code](https://docs.claude.com/en/docs/claude-code/setup) | A small `install.sh` downloads one checksum-verified binary and lets it finish installation; `claude update\|upgrade` checks and installs in one command (observed: 2.1.282 `--help`, and the published `install.sh`). | One pasted command installs; `hamn upgrade` prints one heading, per-artifact progress and one result line. |
| [rustup](https://rust-lang.github.io/rustup/installation/) | [Version summary implementation](https://github.com/rust-lang/rustup/blob/454ff04cdefebc8f38f47f64b3904866f9e0660f/src/cli/common.rs) distinguishes installed, updated, unchanged, and failed results. | Installed-to-selected version display, verified unchanged result, and a success summary only after commit. |

Terminal observations used a PTY: GitHub CLI 2.100.0 `extension upgrade --help`
and `--all --dry-run`, rustup 1.29.1 `update --help`, and Microsoft's installer
`--help` and `--dry-run --version 8.0.100 --architecture arm64 --os osx`.
The GitHub dry-run reported no installed extensions; the .NET dry-run resolved
payload URLs and a repeatable command. Reference tools were not upgraded.
Download and completion patterns above were additionally checked in official
source; they are not claims of observed full reference installations.

Hamn validation uses its real executable, worker, and managed installer under an
isolated HOME. A controlled curl fixture blocks until progress is observed, then
supplies checksum-pinned artifacts. PTY and redirected runs verify progress before
completion, JSON separation, version summaries, and rejected metadata preserving
active state. Update tests also exercise bootstrap TERM/SIGKILL, recovery followed by another failure,
receipt invalidation, repeated releases, and PATH shadowing. Controlled
transport verifies protocol separation and independently counted HTTP fixture bytes;
it does not establish production network throughput or VM boot.
