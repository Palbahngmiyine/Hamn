# Installation and upgrade

See [INSTALLATION.ko.md](INSTALLATION.ko.md) for Korean.

The published installer and updater use macOS system commands, including the
built-in Bash and zsh shells, and support code compiled into the same Hamn
executable. No Python, Perl, Ruby, Homebrew, Rust, or Xcode Command Line Tools
installation is needed on the macOS host. The bootstrap verifies the
archive's pinned SHA-256 with the system `openssl`, reads only its executable
through system `tar` into a private file, then uses that authenticated executable
to validate the complete archive before extracting it. Manifest validation,
receipt compatibility, install locks, recovery metadata and generation collection
run inside Hamn. Before that executable is available, stock `zsh/system` owns
host-download locking and bounded partial writes. Developer builds, release
assembly and tests still require their documented toolchain.

Use the [official installer](../README.md#install) for a first installation or
an incompatible older updater. For a managed installation:

```sh
hamn upgrade --check
hamn upgrade
hamn upgrade --force --output json
hamn --headless system update --yes
hamn --version
```

`hamn update` aliases `hamn upgrade`. Explicit human `upgrade` authorizes the
installation; headless mutations continue to require `--yes`. `--check` fetches
only the manifest and reports `up-to-date`, `repair-required`, `update-available`,
or `ahead` without creating directories, acquiring payloads, recovering a journal,
or changing installation state. Unsupported installations return `unsupported-install`
without network access. `--check` and `--force` conflict. Mutations reject
stable downgrades, development versions, source builds, external package-manager
installations and direct generation binaries; invoke the managed command symlink.
`--manifest URL` selects another HTTPS manifest. Local paths remain test-only.

Human output reports versions and transfer bytes. `--output json` writes one
result object with `schemaVersion`, `currentVersion`, `latestVersion`, `status`,
`downloadedBytes`, `resumedBytes`, `reusedBytes`, per-artifact `artifacts`,
`completed`, and `profileDisksChanged=false`. Headless retains its existing JSON
envelope around this data. Stage messages use stderr. `resumedBytes` describes
the subset of downloaded bytes delivered by a valid Range response; it must not
be added to `downloadedBytes` when calculating total network traffic.

Before changing a generation, the updater validates the manifest, platform,
artifact size (v3), SHA-256 and extracted host version. HTTPS is required for
initial URLs and every redirect. Runtime digest checks do not verify release
pipeline keyless attestations and must not be described as client signature
verification. Schema v2 compatibility omits sizes and uses full downloads.

The existing generation receipt binds the version and host/guest digests to the
installed binary, scripts and packaging. A healthy matching receipt and selected
image produce a no-op with zero payload requests. A damaged guest selection or
image is repaired without replacing a healthy host. `--force` permits same-version
host reinstall while still reusing verified artifacts. Host integrity damage
requires verified host reinstallation. No version string alone authorizes reuse.

Downloads live under `~/.hamn/cache/downloads/`, indexed by SHA-256. Owner-only
locks serialize each digest. Safe partial files resume with Range and a recorded
validator when available; a rejected or ignored Range gets one clean retry.
Size or digest mismatch is never published. The standalone installer's stock-shell
host bootstrap shares the digest lock and verified artifact cache with Hamn's
native downloader. Once the host is authenticated, its native updater acquires
the guest image. No telemetry is sent.
Manual transfers allow 15 seconds to connect and 600 seconds total per request.
Network failures are no longer retried three times automatically; rerun the command
to resume a safe v3 partial. V2 failures restart the full download.

Managed installs collect obsolete generations after commit, retaining the active
and immediately previous generation, open executables/support files, and recovery
references. Install and update transactions serialize on both target roots. A
pending recovery journal, failed process scan, or uncertain ownership defers
collection; retrying installation/update retries cleanup. Interrupted retirement
is also retried. Unmarked directories, incomplete staging copies, external package
manager files, profiles and guest images are outside collection. Do not manually
launch inactive generation paths during collection. Older updater scripts do not
participate in the new transaction locks; finish those before installing this fix.
Generations predating this retention policy are preserved: their recovery roots
in other HOME directories cannot be enumerated. Existing accumulated generations
require separate review; automatic collection bounds new unnecessary generations.


A successful interactive TUI exit may display cached update information and
schedule a detached manifest check. No network is awaited by the TUI, and checks
never run inside an active CLI session or headless/internal commands. Successful
checks have a 24-hour TTL, failed checks a 6-hour backoff, and notices for a version
appear at most once per 24 hours. Checks require stdout/stderr TTYs and a managed
stable installation. `CI` or `HAMN_NO_UPDATE_CHECK=1` disables the automatic path.
An automatic request has a 2-second connect and 5-second total deadline. Cache
files are `~/.hamn/cache/update-check-v1.json` and `update-notice-v1.json`.

The updater serializes recovery and publication and uses a durable journal.
New journal v3 records the exact attempted generation before the installer
publishes its command link. Recovery changes state only when the active target
is the recorded prior or attempted generation; a later installation from another
HOME is preserved. Selection-only repair never rewrites the binary pointer.
Readers still accept v1/v2 journals when the active target remains the recorded
prior generation. If a legacy journal cannot prove ownership of a changed target,
recovery fails without changing either selection or retiring the journal; explicit
inspection of the journal and generation history is required; retrying alone
cannot resolve this ambiguity. Do not delete that
evidence or force a binary rollback to bypass the check. Retry an interrupted
mutation from its original HOME with the same options, including `--manifest`. Recovery can
itself fail and leaves a pending journal that blocks unsafe VM startup. Updates
do not restart VMs or replace existing profile disks. Binary rollback cannot
restore guest state or retired legacy K3s data. First-install failure has no prior
binary to restore; a published command may remain while image selection is restored.

Bootstrap uses a fixed system-tool PATH and preserves the caller's PATH only for
setup advice. It identifies an earlier conflicting `hamn`, and recommends opening
a new terminal after PATH changes. Legacy regular binaries must be migrated to a
managed installation first. A local install pass is not physical VM validation.

## Manifest compatibility and release evidence

The publisher retains strict schema v2 `hamn-update-manifest.json` and adds schema
v3 `hamn-update-manifest-v3.json`, with identical host and guest digests. New
installations point to v3. V3 adds exact artifact sizes, qcow2/zlib guest format and
an 8 GiB virtual size. Manifests are limited to 256 KiB, host archives to 128 MiB,
and guest artifacts to less than 2 GiB. Duplicate and unknown JSON keys are rejected.
V2 consumers additionally accept the previously published valid `repository`
metadata extension. Published historical releases are not rewritten.

Publication requires actual image size evidence and a reviewed
`guest/image/release-size-budget.json`. A review-only report or absent reviewed
budget blocks publication. Hosted validation does not claim physical VM behavior.

## Reference analysis

References were limited to Microsoft, GitHub, and the Rust Foundation ecosystem.
These are interaction choices, not measured usability rankings.

| Official project | Observed pattern | Hamn adaptation |
| --- | --- | --- |
| [GitHub CLI](https://cli.github.com/manual/gh_extension_upgrade) | Operation-specific usage, flags, and dry-run explanation; [update notices use stderr](https://cli.github.com/manual/gh_help_environment). | Update-specific help, explicit `--yes`, progress separate from JSON. |
| [Microsoft .NET installer](https://learn.microsoft.com/en-us/dotnet/core/tools/dotnet-install-script) | [Source](https://github.com/dotnet/install-scripts/blob/47940ac9fc30a2f2dd19167165d0bb0774625f67/src/dotnet-install.sh) reports download, extraction, installed version, PATH advice, and a repeatable dry-run invocation. | Stage messages, actionable retry guidance, conditional PATH advice. |
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
