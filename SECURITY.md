# Security policy

Hamn accepts security reports for the latest published stable release on Apple
Silicon macOS. Unreleased source builds and local test fixtures are not
supported distribution channels.

## Reporting a vulnerability

Use the repository's **Security** tab and its private vulnerability-reporting
form. Do not disclose a vulnerability in a public issue, pull request, log, or
discussion.

If private reporting is temporarily unavailable, open a minimal public issue
requesting a secure reporting channel. Do not include reproduction details,
credentials, paths, diagnostics archives, or proof-of-concept code in that
issue.

## Report contents

Include the published Hamn version, macOS version, Apple Silicon model, a
minimal reproduction, expected and observed behavior, and the potential impact.
Remove Docker registry credentials, kubeconfig credentials, SSH material, and
personal paths before attaching any file.

## Release trust

Install Hamn only with the published installer or a release artifact whose
GitHub attestation and SHA-256 values verify. The published release is
immutable, and the installer and `hamn update` reject incompatible artifacts or
host and guest bytes that do not match the stable manifest digests.
