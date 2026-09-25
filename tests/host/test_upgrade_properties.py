#!/usr/bin/env python3
"""Bounded generated upgrade cases, seed 20260921, against the shipped binary.

These complement the fixed fault/signal matrices and the native Rust property
tests in control/install_support; they are not a proof over all inputs or a
guest boot/functional-equivalence test. All installs, network observations,
processes and files belong to a temporary fixture. No build output is changed.
"""
import contextlib
import hashlib
import json
import os
from pathlib import Path
import random
import selectors
import shlex
import shutil
import signal
import socketserver
import subprocess
import sys
import tarfile
import tempfile
import threading
import unittest

from test_upgrade_native import BINARY, ROOT, manifest

SEED = 20260921


@contextlib.contextmanager
def network_tripwire(root):
    """Observe any attempted TLS connection, independently of curl's path."""
    requests = root / "unexpected-request"

    class Handler(socketserver.BaseRequestHandler):
        def handle(self):
            requests.write_text("unexpected network connection")

    with socketserver.TCPServer(("127.0.0.1", 0), Handler) as server:
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            yield server.server_address[1], requests
        finally:
            server.shutdown()
            worker.join(timeout=2)
            assert not worker.is_alive(), "network observation thread survived"

def private_json(path, value):
    path.write_text(json.dumps(value) + "\n")
    path.chmod(0o600)


def sha(data):
    return hashlib.sha256(data).hexdigest()


class GeneratedTransactions(unittest.TestCase):
    def test_network_tripwire_observes_absolute_curl_connections(self):
        with tempfile.TemporaryDirectory(prefix="hamn-network-tripwire-") as temporary:
            with network_tripwire(Path(temporary)) as (port, requests):
                result = subprocess.run(['/usr/bin/curl', '--disable', '--noproxy', '*',
                    '--connect-timeout', '2', '--max-time', '3',
                    f'https://127.0.0.1:{port}/forbidden'], capture_output=True, timeout=5)
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue(requests.exists(), 'connection observer was insensitive')

    def snapshot(self, profile):
        result = {}
        for path in [profile, *sorted(profile.rglob("*"))]:
            info = path.lstat()
            result[str(path.relative_to(profile))] = (info.st_mode, info.st_ino,
                info.st_size, info.st_mtime_ns, path.read_bytes() if path.is_file() else None)
        return result

    def test_generated_profile_trees_survive_noop_repair_and_recovery(self):
        rng = random.Random(SEED + 3)
        for case, point in enumerate(("PREPARED", "AFTER_GUEST_SELECTION")):
            with self.subTest(seed=SEED + 3, case=case, interruption=point), \
                    tempfile.TemporaryDirectory(prefix="hamn-property-transaction-") as temporary, \
                    contextlib.ExitStack() as services:
                root = Path(temporary).resolve()
                native = root / "native-hamn"
                shutil.copy2(BINARY, native)
                home = root / "home"
                home.mkdir()
                bindir, datadir = home / "bin", home / "source"
                release = root / "release"
                (release / "bin").mkdir(parents=True)
                version = "1." + ".".join(str(rng.randrange(10000)) for _ in range(2))
                # The existing real-binary CLI suite owns frontend coverage.
                # Only the version is generated; all private support operations
                # execute the frozen real native implementation.
                binary = release / "bin/hamn"
                binary.write_text('#!/bin/sh\nif [ "$#" = 1 ] && [ "$1" = --version ]; then\n'
                                  f'  printf "%s\\n" "hamn {version}"\n'
                                  'elif [ "${1:-}" = __install-support ]; then\n'
                                  f'  exec {shlex.quote(str(native))} "$@"\n'
                                  'else exit 64; fi\n')
                binary.chmod(0o755)
                for directory in ("scripts", "packaging"):
                    shutil.copytree(ROOT / directory, release / directory)
                (release / "packaging/release/update-manifest-url").write_text("https://fixture.test/manifest-v3.json\n")
                archive = root / "host.tar.gz"
                with tarfile.open(archive, "w:gz") as bundle:
                    bundle.add(release, arcname="release")
                guest = root / "guest.img"
                guest.write_bytes(rng.randbytes(rng.randrange(1, 8193)))
                value = manifest(guest.read_bytes(), "v" + version)
                for name, artifact in (("host", archive), ("guestImage", guest)):
                    value["artifacts"][name].update(url=artifact.as_uri(),
                        sha256=sha(artifact.read_bytes()), size=artifact.stat().st_size)
                manifest_path = root / "manifest.json"
                private_json(manifest_path, value)
                env = {**os.environ, "HOME": str(home), "TMPDIR": str(root),
                       "HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS": "1"}
                command = bindir / "hamn"

                def invoke(path=manifest_path, success=True, bootstrap=False, extra_env=None):
                    script = ROOT / "scripts/update-host.sh" if bootstrap else command.resolve().parent.parent / "share/hamn/src/scripts/update-host.sh"
                    args = ["bash", script, "--bindir", bindir, "--datadir", datadir,
                            "--manifest", path, "--output-json"]
                    if bootstrap: args.append("--bootstrap")
                    result = subprocess.run(args, env={**env, **(extra_env or {})},
                                            capture_output=True, timeout=30)
                    self.assertEqual(result.returncode == 0, success, result.stderr.decode(errors="replace"))
                    return json.loads(result.stdout) if success else result

                self.assertEqual(invoke(bootstrap=True)["status"], "updated")
                active = os.readlink(command)
                cache = home / ".hamn/cache"
                selection = cache / "guest-image.json"
                desired = selection.read_bytes()
                profiles = []
                for index in range(3):
                    profile = home / ".hamn" / ("profile space " if index == 1 else "profile-한글-" if index == 2 else "profile-")
                    profile.mkdir()
                    (profile / "nested").mkdir()
                    for filename, size in (("disk.img", rng.randrange(1, 16385)), ("config.json", 0),
                                           ("nested/user-data.bin", rng.randrange(1, 4097))):
                        path = profile / filename
                        path.write_bytes(rng.randbytes(size))
                        path.chmod(rng.choice((0o600, 0o640, 0o400)))
                    profiles.append(profile)
                before_profiles = [self.snapshot(profile) for profile in profiles]

                def assert_preserved():
                    self.assertEqual(os.readlink(command), active)
                    self.assertEqual([self.snapshot(profile) for profile in profiles], before_profiles)

                # Native curl has an absolute path. Observe the actual endpoint
                # instead of relying on PATH interception or reported counters.
                port, requests = services.enter_context(network_tripwire(root))
                env.update(NO_PROXY="127.0.0.1", no_proxy="127.0.0.1")
                for name in ("host", "guestImage"):
                    value["artifacts"][name]["url"] = f"https://127.0.0.1:{port}/forbidden/{name}"
                private_json(manifest_path, value)
                result = invoke()
                self.assertEqual(result["status"], "up-to-date")
                self.assertEqual(result["downloadedBytes"], 0)
                self.assertEqual(result["reusedBytes"], archive.stat().st_size + guest.stat().st_size)
                self.assertEqual(selection.read_bytes(), desired)
                self.assertEqual(list(cache.glob(".hamn-update-*")), [])
                assert_preserved()

                if case == 0:
                    selection.unlink()
                    previous = None
                else:
                    old_payload = rng.randbytes(rng.randrange(1, 1025))
                    old_key = sha(old_payload)
                    old_name = f"hamn-guest-{old_key}.img"
                    (cache / old_name).write_bytes(old_payload)
                    (cache / (old_name + ".verified")).write_text(old_key + "\n")
                    private_json(selection, {"schemaVersion": 1, "file": old_name, "sha256": old_key})
                    previous = selection.read_bytes()
                ready, release_fifo = root / "ready", root / "release-fifo"
                os.mkfifo(ready)
                os.mkfifo(release_fifo)
                ready_fd = os.open(ready, os.O_RDWR | os.O_NONBLOCK)
                child = None
                try:
                    script = command.resolve().parent.parent / "share/hamn/src/scripts/update-host.sh"
                    barrier_env = {**env, f"HAMN_TEST_UPDATE_{point}_READY_FIFO": str(ready),
                                   f"HAMN_TEST_UPDATE_{point}_RELEASE_FIFO": str(release_fifo)}
                    child = subprocess.Popen(["bash", script, "--bindir", bindir, "--datadir", datadir,
                        "--manifest", manifest_path, "--output-json"], env=barrier_env,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                    with selectors.DefaultSelector() as selector:
                        selector.register(ready_fd, selectors.EVENT_READ)
                        self.assertTrue(selector.select(20), (point, child.poll()))
                    self.assertEqual(os.read(ready_fd, 64), b"ready\n")
                    journal = cache / ".hamn-update-transaction"
                    self.assertIn("hostMutation=0", (journal / "state").read_text())
                    visible = selection.read_bytes() if selection.exists() else None
                    self.assertEqual(visible, desired if point == "AFTER_GUEST_SELECTION" else previous)
                    assert_preserved()
                    child.send_signal(signal.SIGKILL)
                    child.communicate(timeout=5)
                    self.assertEqual(child.returncode, -signal.SIGKILL)
                    invalid = root / "invalid.json"
                    invalid.write_text("{")
                    invoke(invalid, success=False)
                    self.assertFalse(journal.exists())
                    self.assertEqual(selection.read_bytes() if selection.exists() else None, previous)
                    assert_preserved()
                finally:
                    if child is not None:
                        if child.poll() is None: child.kill()
                        child.communicate(timeout=5)
                    os.close(ready_fd)
                    ready.unlink()
                    release_fifo.unlink()
                repaired = invoke()
                self.assertEqual(repaired["status"], "repaired")
                self.assertEqual(repaired["downloadedBytes"], 0)
                self.assertEqual(selection.read_bytes(), desired)
                self.assertEqual(invoke()["status"], "up-to-date")
                self.assertFalse(requests.exists())
                self.assertEqual(list(cache.glob(".hamn-update-*")), [])
                assert_preserved()


if __name__ == "__main__":
    unittest.main()
