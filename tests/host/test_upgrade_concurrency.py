#!/usr/bin/env python3
"""Serialize real updater transactions without rebuilding the shared executable.

Version wrappers delegate all private operations to a frozen native executable.
The production installer, updater, receipt and journal run in isolated homes;
a fixture-only FIFO observation binds the assertion to root-lock acquisition.
"""
import hashlib
import json
import os
from pathlib import Path
import selectors
import shlex
import shutil
import signal
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
UPDATE = Path(os.environ.get("HAMN_TEST_UPDATER_SOURCE", ROOT / "scripts/update-host.sh"))
HAMN = (ROOT / os.environ.get("HAMN", "build/hamn")).resolve()


class UpgradeConcurrency(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.native_directory = tempfile.TemporaryDirectory(prefix="hamn-concurrency-native-")
        cls.addClassCleanup(cls.native_directory.cleanup)
        cls.native = Path(cls.native_directory.name).resolve() / "hamn"
        shutil.copy2(HAMN, cls.native)
        subprocess.run([cls.native, "__install-support", "upgrade", "version", "1.0.1"],
                       check=True, capture_output=True, timeout=10)

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="hamn-upgrade-concurrency-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.home = self.root / "home"
        self.home.mkdir(mode=0o700)
        self.bindir, self.datadir = self.home / "bin", self.home / "source"
        self.command = self.bindir / "hamn"
        self.selection = self.home / ".hamn/cache/guest-image.json"
        self.env = {**os.environ, "HOME": str(self.home), "TMPDIR": str(self.root),
                    "HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS": "1"}
        self.children = []
        self.addCleanup(self.stop_children)

    def stop_children(self):
        for child in self.children:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL)
            child.communicate(timeout=5)

    def release(self, version):
        release = self.root / ("release-" + version)
        (release / "bin").mkdir(parents=True)
        binary = release / "bin/hamn"
        binary.write_text('#!/bin/sh\nif [ "$#" = 1 ] && [ "$1" = --version ]; then\n'
                          f'  printf "%s\\n" "hamn {version}"\n'
                          'elif [ "${1:-}" = __install-support ]; then\n'
                          f'  exec {shlex.quote(str(self.native))} "$@"\n'
                          'else exit 64; fi\n')
        binary.chmod(0o755)
        for directory in ("scripts", "packaging"):
            shutil.copytree(ROOT / directory, release / directory)
        shutil.copy2(UPDATE, release / "scripts/update-host.sh")
        updater = release / "scripts/update-host.sh"
        source = updater.read_text()
        boundary = 'source "$script_dir/install-transaction.sh"'
        self.assertEqual(source.count(boundary), 1, "missing root-lock fixture boundary")
        observation = ('if [ -n "${HAMN_TEST_UPDATE_BEFORE_LOCK_READY_FIFO:-}" ]; then\n'
                       '  printf "ready\\n" >"$HAMN_TEST_UPDATE_BEFORE_LOCK_READY_FIFO"\n'
                       'fi\n')
        updater.write_text(source.replace(boundary, observation + boundary))
        (release / "packaging/release/update-manifest-url").write_text(
            "https://fixture.test/manifest-v3.json\n")
        archive = self.root / (version + ".tar.gz")
        with tarfile.open(archive, "w:gz") as bundle:
            bundle.add(release, arcname="release")
        guest = self.root / (version + ".img")
        guest.write_bytes(("guest-" + version).encode())
        artifacts = {name: {"url": path.as_uri(), "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                            "size": path.stat().st_size}
                     for name, path in (("host", archive), ("guestImage", guest))}
        artifacts["guestImage"].update(format="qcow2", compression="zlib", virtualSize=8 * 1024**3)
        manifest = self.root / (version + ".json")
        manifest.write_text(json.dumps({"schemaVersion": 3, "channel": "stable", "version": "v" + version,
            "commit": "a" * 40, "validationMode": "github-hosted-no-vm", "artifacts": artifacts,
            "compatibility": {"os": "darwin", "architecture": "arm64", "minimumMacOS": "13.0"}}))
        return release / "scripts/update-host.sh", manifest

    def arguments(self, script, manifest, *options):
        return ["bash", script, "--bindir", self.bindir, "--datadir", self.datadir,
                "--manifest", manifest, "--output-json", *options]

    def run_update(self, script, manifest, *options):
        result = subprocess.run(self.arguments(script, manifest, *options), env=self.env,
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def spawn(self, script, manifest, *options, extra_env):
        child = subprocess.Popen(self.arguments(script, manifest, *options),
                                 env={**self.env, **extra_env}, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, text=True, start_new_session=True)
        self.children.append(child)
        return child

    def ready_fifo(self, name):
        path = self.root / name
        os.mkfifo(path)
        fd = os.open(path, os.O_RDWR | os.O_NONBLOCK)
        self.addCleanup(os.close, fd)
        return path, fd

    def await_ready(self, fd):
        with selectors.DefaultSelector() as selector:
            selector.register(fd, selectors.EVENT_READ)
            self.assertTrue(selector.select(20), "updater did not reach the specified boundary")
        self.assertEqual(os.read(fd, 64), b"ready\n")

    def queued_pair(self, first_script, first_manifest, second_script, second_manifest,
                    first_options=(), second_options=()):
        first_ready, first_fd = self.ready_fifo("first-ready")
        second_ready, second_fd = self.ready_fifo("second-ready")
        release_fifo = self.root / "release-first"
        os.mkfifo(release_fifo)
        first = self.spawn(first_script, first_manifest, *first_options, extra_env={
            "HAMN_TEST_UPDATE_PREPARED_READY_FIFO": str(first_ready),
            "HAMN_TEST_UPDATE_PREPARED_RELEASE_FIFO": str(release_fifo)})
        self.await_ready(first_fd)
        second = self.spawn(second_script, second_manifest, *second_options, extra_env={
            "HAMN_TEST_UPDATE_BEFORE_LOCK_READY_FIFO": str(second_ready)})
        self.await_ready(second_fd)
        # The second invocation reached the root-lock boundary while the first
        # still owns the durable transaction lock. This observation is injected
        # into the release fixture, never into the shipped updater.
        self.assertIsNone(second.poll())
        with release_fifo.open("w") as output:
            output.write("continue\n")
        first_output, first_error = first.communicate(timeout=30)
        self.assertEqual(first.returncode, 0, first_error)
        self.assertEqual(json.loads(first_output)["latestVersion"], "1.0.3")
        active = os.readlink(self.command)
        selected = self.selection.read_bytes()
        second_output, second_error = second.communicate(timeout=30)
        self.assertNotEqual(second.returncode, 0, second_output + second_error)
        self.assertEqual(os.readlink(self.command), active)
        self.assertEqual(self.selection.read_bytes(), selected)
        self.assertEqual(subprocess.check_output([self.command, "--version"], text=True).strip(), "hamn 1.0.3")
        self.assertFalse((self.selection.parent / ".hamn-update-transaction").exists())
        return second_error

    def test_queued_frontend_cannot_replace_newer_active_generation(self):
        initial_script, initial = self.release("1.0.1")
        self.run_update(initial_script, initial, "--bootstrap")
        invoked_script = self.command.resolve().parent.parent / "share/hamn/src/scripts/update-host.sh"
        _, newer = self.release("1.0.3")
        _, older = self.release("1.0.2")
        error = self.queued_pair(invoked_script, newer, invoked_script, older,
                                 ("--current-version", "1.0.1"), ("--current-version", "1.0.1"))
        self.assertIn("managed generation changed", error)

    def test_queued_bootstrap_becomes_an_update_and_rejects_downgrade(self):
        first_script, newer = self.release("1.0.3")
        second_script, older = self.release("1.0.2")
        error = self.queued_pair(first_script, newer, second_script, older,
                                 ("--bootstrap",), ("--bootstrap",))
        self.assertIn("stable downgrade is not permitted", error)

    def test_active_version_overrides_stale_frontend_version(self):
        script, current = self.release("1.0.3")
        self.run_update(script, current, "--bootstrap")
        invoked_script = self.command.resolve().parent.parent / "share/hamn/src/scripts/update-host.sh"
        _, older = self.release("1.0.2")
        active = os.readlink(self.command)
        selection = self.selection.read_bytes()
        result = subprocess.run(self.arguments(invoked_script, older, "--current-version", "1.0.1"),
                                env=self.env, capture_output=True, text=True, timeout=30)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("stable downgrade is not permitted", result.stderr)
        self.assertEqual(os.readlink(self.command), active)
        self.assertEqual(self.selection.read_bytes(), selection)

    def test_writable_runtime_root_rejects_update_without_changing_installation(self):
        script, current = self.release("1.0.1")
        self.run_update(script, current, "--bootstrap")
        invoked_script = self.command.resolve().parent.parent / "share/hamn/src/scripts/update-host.sh"
        _, newer = self.release("1.0.3")
        active, selected = os.readlink(self.command), self.selection.read_bytes()
        (self.home / ".hamn").chmod(0o777)
        result = subprocess.run(self.arguments(invoked_script, newer, "--current-version", "1.0.1"),
                                env=self.env, capture_output=True, text=True, timeout=30)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("unsafe Hamn runtime root", result.stderr)
        self.assertEqual(os.readlink(self.command), active)
        self.assertEqual(self.selection.read_bytes(), selected)
        self.assertFalse((self.selection.parent / ".hamn-update-transaction").exists())


if __name__ == "__main__":
    unittest.main()
