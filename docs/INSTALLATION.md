# Installation and update experience

See [INSTALLATION.ko.md](INSTALLATION.ko.md) for Korean.

Use the [official installer](../README.md#install) for a first installation or
an older updater that cannot read the published release manifest. For a managed
installation, run:

```sh
hamn --headless system update --help
hamn --headless system update --yes
hamn --version
```

The update shows the installed and selected versions, download and verification
stages, and a final version summary. Interactive headless stderr enables download
percentages; redirected stderr and TUI logs use plain stage messages. Headless
stdout remains a JSON result. Bootstrap preserves the caller's PATH only for
setup advice and uses a fixed system-tool PATH for installation. It suggests a
shell PATH entry only if `~/.local/bin` is absent.

Host and guest SHA-256 checks finish before publication. The extracted host
version must match the manifest. The binary and new-profile image selection are
published through the existing recovery journal. Updates do not restart VMs or
replace existing profile disks; the selected image applies to new profile disks.
A binary rollback is not a rollback of guest state or legacy K3s retirement.

On failure, read the diagnostic before retrying. Retry with the same manifest
option, if one was used. An interrupted transaction is recovered before a new
update begins. Recovery can itself fail: the CLI must not promise that the old
binary is active without a successful recovery. Metadata compatibility errors
also link to the official installer. A successful local install is not physical
VM validation; bootstrap metadata records `github-hosted-no-vm`.

## Compatibility contract

Schema v2 publication uses exactly `schemaVersion`, `channel`, `version`,
`commit`, `validationMode`, `compatibility`, and `artifacts`. Previously published
v0.1.x manifests also include `repository`; the updater accepts this optional
`owner/name` metadata while rejecting other unknown fields and invalid values.
Repository metadata is descriptive, not a replacement for artifact verification.
Keeping the producer's original key set lets older strict v2 updaters consume
future releases. This source change does not rewrite already published releases.

## Reference analysis

References were limited to Microsoft, GitHub, and the Rust Foundation ecosystem.
These are interaction choices, not measured usability rankings.

| Official project | Observed pattern | Hamn adaptation |
| --- | --- | --- |
| [GitHub CLI](https://cli.github.com/manual/gh_extension_upgrade) | Operation-specific usage, flags, and dry-run explanation; [update notices use stderr](https://cli.github.com/manual/gh_help_environment). | Update-specific help, explicit `--yes`, progress separate from JSON. |
| [Microsoft .NET installer](https://learn.microsoft.com/en-us/dotnet/core/tools/dotnet-install-script) | [Source](https://github.com/dotnet/install-scripts/blob/47940ac9fc30a2f2dd19167165d0bb0774625f67/src/dotnet-install.sh) reports download, extraction, installed version, PATH advice, and a repeatable dry-run invocation. | Stage messages, actionable retry guidance, conditional PATH advice. |
| [rustup](https://rust-lang.github.io/rustup/installation/) | [Version summary implementation](https://github.com/rust-lang/rustup/blob/454ff04cdefebc8f38f47f64b3904866f9e0660f/src/cli/common.rs) distinguishes installed, updated, unchanged, and failed results. | Installed-to-selected version display and a success summary only after commit. |

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
active state. Existing update tests exercise interruption and rollback. Controlled
transport verifies progress-mode selection, not real network throughput or VM boot.
