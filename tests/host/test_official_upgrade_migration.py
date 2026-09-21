#!/usr/bin/env python3
"""Exercise the exact published v0.1.2 client against an unpublished candidate.

Fetch the immutable official archive separately and pass --old-archive. Its
pinned digest is checked before extraction or execution. This test uses local
candidate manifests and a synthetic guest image, never a public release update
or a VM boot. It does not claim attestation or physical runtime validation.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[2]
OLD_SHA256 = "fbeabb3d8efc8cb6b714e085dbb89a29f6409d2b73d64738c98d32649a843e8b"
OLD_SOURCE = "3b7ab8ee7ce13b220c8eea0e0cbfe713634b613e"
OLD_URL = ("https://github.com/Palbahngmiyine/Hamn/releases/download/v0.1.2/"
           "hamn-v0.1.2-darwin-arm64.tar.gz")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(args):
    old_archive = args.old_archive.resolve()
    assert digest(old_archive) == OLD_SHA256, "official v0.1.2 archive digest mismatch"
    with tempfile.TemporaryDirectory(prefix="hamn-official-migration-") as temporary:
        root = Path(temporary).resolve()
        home, scratch = root / "home", root / "scratch"
        home.mkdir(mode=0o700)
        scratch.mkdir(mode=0o700)
        native = root / "candidate-hamn"
        shutil.copy2(args.hamn.resolve(), native)
        candidate_sha256 = digest(native)
        env = {**os.environ, "HOME": str(home), "TMPDIR": str(scratch),
               "HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS": "1", "HAMN_NO_UPDATE_CHECK": "1"}
        steps = []

        def command(label, *words):
            result = subprocess.run(list(map(str, words)), env=env, capture_output=True,
                                    text=True, timeout=90)
            assert result.returncode == 0, (label, result.returncode, result.stdout, result.stderr)
            steps.append({"step": label, "stdout": result.stdout, "stderr": result.stderr})
            return result.stdout

        version = command("candidate-version", native, "--version").strip().removeprefix("hamn ")
        assert tuple(map(int, version.split("."))) >= (0, 1, 2), "candidate must not downgrade v0.1.2"
        extracted = root / "official"
        artifact_root = command("extract-verified-official-archive", native, "__install-support",
                                "extract", old_archive, extracted).strip()
        official = extracted / artifact_root
        official_binary = official / "bin/hamn"
        assert command("official-version", official_binary, "--version").strip() == "hamn 0.1.2"
        original_updater = official / "scripts/update-host.sh"
        original_updater_sha256 = digest(original_updater)
        binary_sha256 = digest(official_binary)
        bindir, datadir = home / ".local/bin", home / ".local/share/hamn/src"
        # Install using the archived installer and archived helper, unchanged.
        command("official-install", "/bin/bash", official / "scripts/install-host.sh",
                official_binary, bindir, datadir)
        installed = bindir / "hamn"
        initial = installed.resolve()
        assert digest(initial) == binary_sha256
        installed_updater = initial.parent.parent / "share/hamn/src/scripts/update-host.sh"
        assert digest(installed_updater) == original_updater_sha256

        runtime = home / ".hamn"
        runtime.mkdir(mode=0o700, exist_ok=True)
        preserved = runtime / "profiles/preserved"
        preserved.mkdir(parents=True, mode=0o700)
        for name, data in {"disk.img": b"existing profile disk sentinel\x00",
                           "config.yaml": b"name: preserved\ncpus: 2\n",
                           "user-data": b"unchanged user data\n"}.items():
            (preserved / name).write_bytes(data)
        snapshot = {str(p.relative_to(preserved)): digest(p) for p in preserved.rglob("*") if p.is_file()}

        candidate = root / "candidate"
        (candidate / "bin").mkdir(parents=True)
        shutil.copy2(native, candidate / "bin/hamn")
        for name in ("scripts", "packaging"):
            shutil.copytree(ROOT / name, candidate / name,
                            ignore=shutil.ignore_patterns("__pycache__", "*.pyc", ".DS_Store"))
        manifest_v2, manifest_v3 = root / "manifest-v2.json", root / "manifest-v3.json"
        # The candidate's next default lookup moves to the v3 endpoint. Only
        # this isolated test uses file URLs; shipped pointers remain HTTPS.
        (candidate / "packaging/release/update-manifest-url").write_text(manifest_v3.as_uri() + "\n")
        host, guest = root / "candidate.tar.gz", root / "guest.img"
        with tarfile.open(host, "w:gz") as bundle:
            bundle.add(candidate, arcname="candidate")
        guest.write_bytes(b"synthetic guest image: migration tests never start a VM\n")
        manifest = {"schemaVersion": 2, "channel": "stable", "version": "v" + version,
                    "commit": "a" * 40, "validationMode": "github-hosted-no-vm",
                    "compatibility": {"os": "darwin", "architecture": "arm64", "minimumMacOS": "13.0"},
                    "artifacts": {name: {"url": path.as_uri(), "sha256": digest(path)}
                                  for name, path in (("host", host), ("guestImage", guest))}}
        manifest_v2.write_text(json.dumps(manifest))
        manifest["schemaVersion"] = 3
        for name, path in (("host", host), ("guestImage", guest)):
            manifest["artifacts"][name]["size"] = path.stat().st_size
        manifest["artifacts"]["guestImage"].update(format="qcow2", compression="zlib", virtualSize=8 * 1024**3)
        manifest_v3.write_text(json.dumps(manifest))

        # This is the published executable's public CLI, not a frozen parser
        # approximation or a copy of the candidate's updater invoked directly.
        command("official-cli-consumes-candidate-v2", installed, "--headless", "system", "update",
                "--yes", "--manifest", manifest_v2)
        migrated = installed.resolve()
        assert migrated != initial and digest(migrated) == candidate_sha256
        selected = runtime / "cache/guest-image.json"
        selection = selected.read_bytes()
        check = json.loads(command("candidate-default-v3-check", installed, "upgrade", "--check", "--output", "json"))
        assert check["status"] == "up-to-date", check
        assert installed.resolve() == migrated and selected.read_bytes() == selection
        forced = json.loads(command("candidate-default-v3-force", installed, "upgrade", "--force", "--output", "json"))
        assert forced["status"] == "updated", forced
        replacement = installed.resolve()
        assert replacement != migrated and digest(replacement) == candidate_sha256
        unchanged = json.loads(command("candidate-v3-alias-noop", installed, "update", "--output", "json"))
        assert unchanged["status"] == "up-to-date" and unchanged["downloadedBytes"] == 0, unchanged
        assert installed.resolve() == replacement and selected.read_bytes() == selection
        assert not (runtime / "cache/.hamn-update-transaction").exists()
        assert {str(p.relative_to(preserved)): digest(p) for p in preserved.rglob("*") if p.is_file()} == snapshot
        assert digest(official_binary) == binary_sha256 and digest(original_updater) == original_updater_sha256
        return {"officialVersion": "0.1.2", "officialSource": OLD_SOURCE, "officialUrl": OLD_URL,
                "officialArchiveSHA256": OLD_SHA256, "officialBinarySHA256": binary_sha256,
                "officialUpdaterSHA256": original_updater_sha256, "candidateVersion": version,
                "candidateBinarySHA256": candidate_sha256, "candidateArchiveSHA256": digest(host),
                "candidateV2SHA256": digest(manifest_v2), "candidateV3SHA256": digest(manifest_v3),
                "profileBytesPreserved": True, "publicLatestEndpointTested": False,
                "guestRuntimeTested": False, "attestationVerifiedByThisTest": False,
                "steps": steps}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--old-archive", type=Path, required=True)
    parser.add_argument("--hamn", type=Path, default=ROOT / "build/hamn")
    parser.add_argument("--evidence", type=Path)
    args = parser.parse_args()
    evidence = run(args)
    rendered = json.dumps(evidence, indent=2) + "\n"
    if args.evidence:
        args.evidence.write_text(rendered)
    print(rendered, end="")


if __name__ == "__main__":
    main()
