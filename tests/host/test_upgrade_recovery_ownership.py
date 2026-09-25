#!/usr/bin/env python3
"""A pending transaction may restore only the generation it actually published.

Real installer/updater scripts and frozen native support run in owned homes.
FIFO boundaries and process-group SIGKILL make interruptions reproducible; no
VM, release network, or shared build is involved.
"""
import hashlib
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import tarfile
import unittest

import test_upgrade_concurrency as concurrency


class RecoveryOwnership(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        concurrency.UpgradeConcurrency.setUpClass()
        cls.addClassCleanup(concurrency.UpgradeConcurrency.doClassCleanups)

    def setUp(self):
        self.fixture = concurrency.UpgradeConcurrency()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)

    def snapshot(self, directory):
        return {str(path.relative_to(directory)): (
            path.lstat().st_mode, path.lstat().st_mtime_ns,
            hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None)
            for path in sorted(directory.rglob("*"))}

    def interrupt(self, manifest, point):
        f = self.fixture
        ready, fd = f.ready_fifo("ready")
        release = f.root / "release-fifo"
        os.mkfifo(release)
        script = f.command.resolve().parent.parent / "share/hamn/src/scripts/update-host.sh"
        child = f.spawn(script, manifest, extra_env={
            f"HAMN_TEST_UPDATE_{point}_READY_FIFO": str(ready),
            f"HAMN_TEST_UPDATE_{point}_RELEASE_FIFO": str(release)})
        f.await_ready(fd)
        os.killpg(child.pid, signal.SIGKILL)
        child.communicate(timeout=5)
        self.assertEqual(child.returncode, -signal.SIGKILL)
        return f.selection.parent / ".hamn-update-transaction"

    def recover(self, script=None, bootstrap=False):
        f = self.fixture
        invalid = f.root / "invalid.json"
        invalid.write_text("{")
        if script is None:
            script = f.command.resolve().parent.parent / "share/hamn/src/scripts/update-host.sh"
        options = ("--bootstrap",) if bootstrap else ()
        return subprocess.run(f.arguments(script, invalid, *options), env=f.env,
                              capture_output=True, text=True, timeout=30)

    def assert_later_home_survives_old_recovery(self, host_mutation):
        f = self.fixture
        script, initial = f.release("1.0.1")
        f.run_update(script, initial, "--bootstrap")
        _, pending = f.release("1.0.2")
        _, later = f.release("1.0.3")
        if not host_mutation:
            f.selection.unlink()
            pending = initial
        journal = self.interrupt(pending, "AFTER_GUEST_SELECTION")
        self.assertIn(f"hostMutation={int(host_mutation)}", (journal / "state").read_text())
        before_a = self.snapshot(f.selection.parent)
        other = f.root / "other-home"
        other.mkdir(mode=0o700)
        script = f.command.resolve().parent.parent / "share/hamn/src/scripts/update-host.sh"
        result = subprocess.run(f.arguments(script, later), env={**f.env, "HOME": str(other)},
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        if host_mutation:
            # Move beyond the immediate predecessor, so preservation depends on
            # the attempted generation's pending recovery-root reference.
            _, newest = f.release("1.0.4")
            script = f.command.resolve().parent.parent / "share/hamn/src/scripts/update-host.sh"
            result = subprocess.run(f.arguments(script, newest), env={**f.env, "HOME": str(other)},
                                    capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(Path((journal / "new-target").read_text().strip()).is_file())
        active = os.readlink(f.command)
        before_b = self.snapshot(other / ".hamn/cache")
        self.assertEqual(self.snapshot(f.selection.parent), before_a)
        result = self.recover()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(os.readlink(f.command), active,
                         "stale HOME journal rolled back a successful later generation")
        self.assertEqual(self.snapshot(f.selection.parent), before_a,
                         "ambiguous recovery changed its selection/cache or retired its journal")
        self.assertEqual(self.snapshot(other / ".hamn/cache"), before_b)
        self.assertIn("does not own the active generation", result.stderr)
        self.assertIn("manual review", result.stderr)
        self.assertIn("retrying alone will not resolve", result.stderr)
        self.assertTrue(journal.is_dir())

    def test_later_home_host_update_survives_prior_host_transaction(self):
        self.assert_later_home_survives_old_recovery(True)

    def test_later_home_host_update_survives_prior_selection_only_transaction(self):
        self.assert_later_home_survives_old_recovery(False)

    def legacy_recovery(self, version, changed_active):
        f = self.fixture
        script, initial = f.release("1.0.1")
        f.run_update(script, initial, "--bootstrap")
        original = os.readlink(f.command)
        selection = f.selection.read_bytes()
        _, pending = f.release("1.0.2")
        point = "AFTER_GUEST_SELECTION" if changed_active else "PREPARED"
        journal = self.interrupt(pending, point)
        state = (journal / "state").read_text().replace("version=3", f"version={version}")
        if version == 1:
            state = state.replace("hostMutation=1\n", "")
        (journal / "state").write_text(state)
        (journal / "new-target").unlink()
        active = os.readlink(f.command)
        snapshot = self.snapshot(f.selection.parent)
        result = self.recover()
        self.assertNotEqual(result.returncode, 0)  # invalid manifest, after recovery
        if changed_active:
            self.assertEqual(os.readlink(f.command), active)
            self.assertEqual(self.snapshot(f.selection.parent), snapshot)
            self.assertIn("does not own the active generation", result.stderr)
            self.assertTrue(journal.exists())
        else:
            self.assertEqual(os.readlink(f.command), original)
            self.assertEqual(f.selection.read_bytes(), selection)
            self.assertIn("recovered the previous binary", result.stderr)
            self.assertFalse(journal.exists())

    def test_legacy_v1_unchanged_active_remains_recoverable(self):
        self.legacy_recovery(1, False)

    def test_legacy_v2_unchanged_active_remains_recoverable(self):
        self.legacy_recovery(2, False)

    def test_legacy_v1_changed_active_is_preserved_as_ambiguous(self):
        self.legacy_recovery(1, True)

    def test_legacy_v2_changed_active_is_preserved_as_ambiguous(self):
        self.legacy_recovery(2, True)

    def publication_interruption(self, bootstrap):
        f = self.fixture
        original, selected = None, None
        if not bootstrap:
            script, initial = f.release("1.0.1")
            f.run_update(script, initial, "--bootstrap")
            original, selected = os.readlink(f.command), f.selection.read_bytes()
        script, pending = f.release("1.0.2")
        ready, fd = f.ready_fifo("before-publication")
        release = f.root / "before-publication-release"
        os.mkfifo(release)
        installer = script.parent / "install-host.sh"
        source = installer.read_text()
        boundary = 'hamn_link_stage=$(make_link_stage .hamn-link "$generation/bin/hamn")'
        self.assertEqual(source.count(boundary), 1)
        barrier = (f'printf "ready\\n" > {shlex.quote(str(ready))}\n'
                   f'IFS= read -r _ < {shlex.quote(str(release))}\n')
        installer.write_text(source.replace(boundary, barrier + boundary))
        value = json.loads(pending.read_text())
        archive = Path(value["artifacts"]["host"]["url"].removeprefix("file://"))
        with tarfile.open(archive, "w:gz") as bundle:
            bundle.add(script.parent.parent, arcname="release")
        value["artifacts"]["host"].update(size=archive.stat().st_size,
            sha256=hashlib.sha256(archive.read_bytes()).hexdigest())
        pending.write_text(json.dumps(value))
        invoked = script if bootstrap else f.command.resolve().parent.parent / "share/hamn/src/scripts/update-host.sh"
        options = ("--bootstrap",) if bootstrap else ()
        child = f.spawn(invoked, pending, *options, extra_env={})
        f.await_ready(fd)
        journal = f.selection.parent / ".hamn-update-transaction"
        attempted = Path((journal / "new-target").read_text().strip())
        self.assertNotEqual(str(attempted), original)
        self.assertTrue(attempted.is_file())
        self.assertEqual(os.readlink(f.command) if f.command.is_symlink() else None, original)
        cache_id = hashlib.sha256(str(f.selection.parent.resolve()).encode()).hexdigest()
        self.assertEqual((attempted.parent.parent / (".hamn-recovery-root-" + cache_id)).read_text(),
                         str(f.selection.parent.resolve()))
        os.killpg(child.pid, signal.SIGKILL)
        child.communicate(timeout=5)
        result = self.recover(script=invoked, bootstrap=bootstrap)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Recovered interrupted bootstrap" if bootstrap else "recovered the previous binary", result.stderr)
        self.assertEqual(os.readlink(f.command) if f.command.is_symlink() else None, original)
        self.assertEqual(f.selection.read_bytes() if f.selection.exists() else None, selected)
        self.assertFalse(journal.exists())

    def test_target_is_durable_before_link_publication_and_recovers_after_kill(self):
        self.publication_interruption(False)

    def test_first_bootstrap_recovers_recorded_target_before_link_publication(self):
        self.publication_interruption(True)

    def test_recovery_failure_keeps_exact_target_for_next_retry(self):
        f = self.fixture
        script, initial = f.release("1.0.1")
        f.run_update(script, initial, "--bootstrap")
        original, selected = os.readlink(f.command), f.selection.read_bytes()
        _, pending = f.release("1.0.2")
        journal = self.interrupt(pending, "AFTER_GUEST_SELECTION")
        attempted = os.readlink(f.command)
        self.assertEqual((journal / "new-target").read_text(), attempted + "\n")
        transport = f.root / "transport"
        transport.mkdir()
        move = transport / "mv"
        move.write_text('#!/bin/bash\nfor arg in "$@"; do\n'
                        'case "$arg" in */.hamn-update-rollback.*/hamn) exit 74;; esac\n'
                        'done\nexec /bin/mv "$@"\n')
        move.chmod(0o755)
        f.env["HAMN_TEST_UPDATE_TOOL_DIR"] = str(transport)
        failed = self.recover()
        self.assertNotEqual(failed.returncode, 0)
        self.assertIn("could not be safely recovered", failed.stderr)
        self.assertEqual(os.readlink(f.command), attempted)
        self.assertEqual((journal / "new-target").read_text(), attempted + "\n")
        move.unlink()
        recovered = self.recover()
        self.assertNotEqual(recovered.returncode, 0)
        self.assertIn("recovered the previous binary", recovered.stderr)
        self.assertEqual(os.readlink(f.command), original)
        self.assertEqual(f.selection.read_bytes(), selected)
        self.assertFalse(journal.exists())

    def test_installer_rejects_foreign_journal_without_writing_it(self):
        f = self.fixture
        script, initial = f.release("1.0.1")
        f.run_update(script, initial, "--bootstrap")
        active = os.readlink(f.command)
        foreign = f.root / "foreign-journal"
        foreign.mkdir(mode=0o700)
        (foreign / "sentinel").write_bytes(b"must remain unchanged")
        before = self.snapshot(foreign)
        generations = set(f.datadir.joinpath(".hamn-generations").iterdir())
        result = subprocess.run(["bash", script.parent / "install-host.sh",
            script.parent.parent / "bin/hamn", f.bindir, f.datadir, foreign],
            env=f.env, capture_output=True, text=True, timeout=30)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsafe update journal handoff", result.stderr)
        self.assertEqual(self.snapshot(foreign), before)
        self.assertEqual(os.readlink(f.command), active)
        self.assertEqual(set(f.datadir.joinpath(".hamn-generations").iterdir()), generations)


if __name__ == "__main__":
    # CI runs the legacy-journal tests and the others as separate gates; every
    # test is in exactly one part, and no argument runs both.
    parts = {"legacy": True, "current": False}
    selected = [parts[part] for part in sys.argv[1:]] or list(parts.values())
    names = [name for name in unittest.TestLoader().getTestCaseNames(RecoveryOwnership)
             if ("_legacy_" in name) in selected]
    result = unittest.TextTestRunner().run(unittest.TestSuite(map(RecoveryOwnership, names)))
    raise SystemExit(not result.wasSuccessful())
