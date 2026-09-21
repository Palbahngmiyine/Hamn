#!/usr/bin/env python3
"""Real managed-install/PTY notice checks and isolated checker policy tests.

Never rebuilds, runs a VM or accesses a release service. The native checker uses an invalid offline manifest URL, so dispatch is observed
through its owned lock/cache files without release network. Native Rust tests
cover sanitized launcher argv/environment/stdio and controlled policy boundaries;
the historical Python policy oracle remains an independent comparison.
"""
import argparse
import fcntl
import importlib.util
import json
import multiprocessing
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unittest
from unittest.mock import patch

from terminal_screen import RatatuiScreen

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "scripts/upgrade_support.py"
spec = importlib.util.spec_from_file_location("update_check_helper", HELPER)
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


def write_json(path, value, mode=0o600):
    path.write_text(json.dumps(value) + "\n")
    path.chmod(mode)


def settings(fd):
    value = termios.tcgetattr(fd)
    value[3] &= ~getattr(termios, "PENDIN", 0)
    return value


class ManagedNotice(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.work = tempfile.TemporaryDirectory(prefix="hamn-check-", dir="/tmp")
        cls.addClassCleanup(cls.work.cleanup)
        cls.root = Path(cls.work.name).resolve()
        binary = Path(os.environ.get("HAMN", ROOT / "build/hamn")).resolve()
        cls.frozen = cls.root / "hamn-source"
        shutil.copy2(binary, cls.frozen)
        cls.frozen.chmod(0o755)
        cls.version = subprocess.check_output([cls.frozen, "--version"], text=True, timeout=5).strip().split()[1]
        major = int(cls.version.removeprefix("v").split(".")[0]) + 1
        cls.latest = f"{major}.0.0"
        cls.bindir, cls.datadir = cls.root / "bin", cls.root / "src"
        subprocess.run(["bash", ROOT / "scripts/install-host.sh", cls.frozen, cls.bindir, cls.datadir],
                       check=True, capture_output=True, timeout=30,
                       env={**os.environ, "HOME": str(cls.root)})
        cls.managed = cls.bindir / "hamn"
        cls.generation = cls.managed.resolve().parent.parent
        source = cls.generation / "share/hamn/src"
        (source / "packaging/release/update-manifest-url").write_text("invalid-offline-url\n")
        # A normal install must check updates without an external language helper.
        (source / "scripts/upgrade_support.py").unlink(missing_ok=True)
        cls.tools = cls.root / "tools"
        cls.tools.mkdir()
        cli = '''#!/usr/bin/python3
import json, sys
if "context" in sys.argv:
    print(json.dumps({"Name":"external","DockerEndpoint":"unix:///unused","Current":True}))
elif "ps" in sys.argv:
    print(json.dumps({"ID":"abcdef123456","Names":"notice-fixture","State":"running"}))
elif "get" in sys.argv:
    print('{"items":[]}')
'''
        for name in ("docker", "kubectl"):
            path = cls.tools / name
            path.write_text(cli)
            path.chmod(0o755)
        cls.sequence = 0

    def setUp(self):
        type(self).sequence += 1
        self.home = self.root / f"h{self.sequence}"
        self.home.mkdir(mode=0o700)
        self.runtime = self.home / ".hamn"
        self.runtime.mkdir(mode=0o700)
        self.cache = self.runtime / "cache"
        self.cache.mkdir(mode=0o755)
        self.check_path = self.cache / "update-check-v1.json"
        self.notice_path = self.cache / "update-notice-v1.json"
        write_json(self.runtime / "tui.json", {
            "version": 1, "defaultWorkspace": "containers",
            "recentTargets": [{"kind": "docker", "name": "external", "config": None}],
        })
        self.now = int(time.time())
        self.check_record = {"schemaVersion": 1, "checkedAt": self.now,
                             "ok": True, "latestVersion": self.latest}
        write_json(self.check_path, self.check_record)
        self.env = dict(os.environ, HOME=str(self.home), TERM="xterm-256color",
                        PATH=f"{self.tools}:/usr/bin:/bin", KUBECONFIG=str(self.home / "no-kubeconfig"),
                        HAMN_UPDATE_CHECK_TEST_SECRET="must-not-reach-scheduler")
        self.env.pop("CI", None)
        self.env.pop("HAMN_NO_UPDATE_CHECK", None)

    def run_tui(self, binary=None, extra_env=None, schedule=False, stderr_tty=True, arguments=()):
        lock_path = self.cache / ".update-check.lock"
        # Each invocation needs a new creation witness. A prior worker must have
        # released its actual cross-process lock before this fixture removes it.
        if lock_path.exists():
            with lock_path.open("r+") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                lock_path.unlink()
        result = self.terminal([binary or self.managed, *arguments],
                               {**self.env, **(extra_env or {})}, stderr_tty,
                               interactive=not arguments)
        deadline = time.monotonic() + (5 if schedule else 0.5)
        while not lock_path.exists() and time.monotonic() < deadline:
            select.select([], [], [], 0.01)
        self.assertEqual(lock_path.exists(), schedule, result[-2000:])
        if schedule:
            # Creating the lock precedes acquiring it. Observe the exact owned
            # executable's checker exit instead of winning that acquisition race.
            prefix = str(self.managed.resolve()) + " __install-support upgrade schedule "
            while True:
                processes = subprocess.check_output(["/bin/ps", "-axo", "args="], text=True, timeout=2)
                if not any(line.startswith(prefix) for line in processes.splitlines()):
                    break
                self.assertLess(time.monotonic(), deadline, "native checker did not finish")
                select.select([], [], [], 0.01)
        return result

    def terminal(self, command, env, stderr_tty, interactive=True):
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 130, 0, 0))
        before = settings(slave)
        child = subprocess.Popen(command, stdin=slave, stdout=slave,
                                 stderr=slave if stderr_tty else subprocess.PIPE,
                                 env=env, start_new_session=True)
        output = bytearray()
        screen = RatatuiScreen(30, 130)
        try:
            if interactive:
                deadline = time.monotonic() + 10
                while "notice-fixture" not in screen.text():
                    remaining = deadline - time.monotonic()
                    self.assertGreater(remaining, 0, screen.text())
                    self.assertTrue(select.select([master], [], [], remaining)[0], screen.text())
                    data = os.read(master, 65536)
                    self.assertTrue(data, screen.text())
                    output.extend(data)
                    screen.feed(data)
                os.write(master, b"q")
            deadline = time.monotonic() + 5
            while child.poll() is None:
                remaining = deadline - time.monotonic()
                self.assertGreater(remaining, 0, bytes(output[-2000:]))
                if select.select([master], [], [], min(remaining, 0.1))[0]:
                    output.extend(os.read(master, 65536))
            self.assertEqual(child.wait(timeout=2), 0, bytes(output[-2000:]))
            while select.select([master], [], [], 0)[0]:
                output.extend(os.read(master, 65536))
            if child.stderr is not None:
                output.extend(child.stderr.read())
                child.stderr.close()
            self.assertEqual(settings(slave), before)
            if interactive:
                self.assertIn(b"\x1b[?1049l", output)
            return bytes(output)
        finally:
            if child.poll() is None: os.killpg(child.pid, signal.SIGKILL)
            child.wait(timeout=5)
            os.close(master)
            os.close(slave)

    def assert_no_notice(self, output):
        self.assertNotIn(b"is available; run hamn upgrade.", output)

    def test_cached_notice_after_restore_and_24_hour_throttle(self):
        output = self.run_tui()
        notice = f"Hamn {self.latest} is available; run hamn upgrade.".encode()
        self.assertIn(notice, output)
        self.assertLess(output.rfind(b"\x1b[?1049l"), output.find(notice))
        first = self.notice_path.read_bytes()
        self.assertEqual(stat.S_IMODE(self.notice_path.stat().st_mode), 0o600)
        self.assert_no_notice(self.run_tui())
        self.assertEqual(self.notice_path.read_bytes(), first)
        value = json.loads(first)
        value["shownAt"] = int(time.time()) - 86460
        write_json(self.notice_path, value)
        self.assertIn(notice, self.run_tui("hamn", {"PATH": f"{self.bindir}:{self.env['PATH']}"}))

    def test_stale_cache_schedules_with_clean_environment_and_no_network(self):
        write_json(self.check_path, {**self.check_record, "checkedAt": self.now - 86460})
        self.assertIn(b"run hamn upgrade.", self.run_tui(schedule=True))
        self.assertEqual(set(path.name for path in self.runtime.iterdir()), {"cache", "tui.json", "tui.lock"})

    def test_first_check_can_be_scheduled_before_cache_directory_exists(self):
        self.check_path.unlink()
        self.cache.rmdir()
        self.assert_no_notice(self.run_tui(schedule=True))

    def test_unsafe_cache_directory_blocks_notices_and_scheduling(self):
        write_json(self.check_path, {**self.check_record, "checkedAt": self.now - 86460})
        original = self.check_path.read_bytes()
        external = self.home / "outside-directory"
        self.cache.rename(external)
        self.cache.symlink_to(external, target_is_directory=True)
        self.assert_no_notice(self.run_tui())
        self.assertFalse((external / "update-notice-v1.json").exists())
        self.assertEqual((external / self.check_path.name).read_bytes(), original)
        self.cache.unlink()
        external.rename(self.cache)
        self.cache.chmod(0o777)
        self.assert_no_notice(self.run_tui())
        self.assertFalse(self.notice_path.exists())
        self.assertEqual(self.check_path.read_bytes(), original)

    def test_source_direct_generation_ci_opt_out_and_stderr_exclusions(self):
        # Stale data would cause both notice and scheduling if eligibility leaks.
        write_json(self.check_path, {**self.check_record, "checkedAt": self.now - 86460})
        for binary, env, tty in ((self.frozen, {}, True), (self.managed.resolve(), {}, True),
                                 (self.managed, {"CI": "1"}, True),
                                 (self.managed, {"HAMN_NO_UPDATE_CHECK": "1"}, True),
                                 (self.managed, {}, False)):
            with self.subTest(binary=binary, env=env, stderr_tty=tty):
                self.assert_no_notice(self.run_tui(binary, env, stderr_tty=tty))
                self.assertFalse(self.notice_path.exists())

    def test_headless_and_non_tty_keep_original_results(self):
        write_json(self.check_path, {**self.check_record, "checkedAt": self.now - 86460})
        before = self.check_path.read_bytes()
        # Keep both output descriptors attached to the terminal so this checks
        # headless exclusion independently of the non-TTY guard.
        output = self.run_tui(arguments=("--headless", "capabilities"))
        self.assertTrue(json.loads(output)["ok"])
        self.assert_no_notice(output)
        result = subprocess.run([self.managed, "--headless", "capabilities"], env=self.env,
                                capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(json.loads(result.stdout)["ok"])
        self.assert_no_notice(result.stdout + result.stderr)
        result = subprocess.run([self.managed], env=self.env, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(b"terminal is required", result.stdout)
        self.assert_no_notice(result.stdout + result.stderr)
        self.assertEqual(self.check_path.read_bytes(), before)
        self.assertFalse(self.notice_path.exists())

    def test_corrupt_future_unsafe_and_linked_check_cache_cannot_change_tui_result(self):
        external = self.home / "outside-cache"
        write_json(external, self.check_record)
        for kind in ("malformed", "duplicate", "future", "mode", "symlink", "hardlink"):
            with self.subTest(kind=kind):
                self.check_path.unlink(missing_ok=True)
                if kind == "malformed":
                    self.check_path.write_text("{"); self.check_path.chmod(0o600)
                elif kind == "duplicate":
                    self.check_path.write_text('{"schemaVersion":1,"checkedAt":0,"ok":true,"latestVersion":"1.0.0","latestVersion":"9.0.0"}')
                    self.check_path.chmod(0o600)
                elif kind == "future":
                    write_json(self.check_path, {**self.check_record, "checkedAt": self.now + 3600})
                elif kind == "mode":
                    write_json(self.check_path, self.check_record, 0o644)
                elif kind == "symlink": self.check_path.symlink_to(external)
                else: os.link(external, self.check_path)
                before = self.check_path.read_bytes()
                self.assert_no_notice(self.run_tui(schedule=True))
                if kind in ("mode", "symlink", "hardlink"):
                    self.assertEqual(self.check_path.read_bytes(), before)
                else:
                    refreshed = json.loads(self.check_path.read_text())
                    self.assertFalse(refreshed["ok"])
                    self.assertGreaterEqual(refreshed["checkedAt"], self.now)
                self.assertEqual(json.loads(external.read_text()), self.check_record)
                self.assertFalse(self.notice_path.exists())

    def test_unsafe_notice_target_is_preserved(self):
        external = self.home / "notice-sentinel"
        external.write_text("preserve")
        self.notice_path.symlink_to(external)
        self.assert_no_notice(self.run_tui())
        self.assertTrue(self.notice_path.is_symlink())
        self.assertEqual(external.read_text(), "preserve")


class CheckerPolicy(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory(prefix="hamn-check-policy-")
        self.addCleanup(self.work.cleanup)
        self.home = Path(self.work.name)
        self.cache = helper.cache_root(self.home)
        self.args = argparse.Namespace(home=str(self.home), current_version="1.2.3", manifest="https://unused.invalid/manifest", macos="13.0", architecture="arm64")
        self.now = 1000000
        self.record = {"schemaVersion": 1, "checkedAt": self.now, "ok": True, "latestVersion": "1.3.0"}

    def test_success_ttl_and_failure_backoff_boundaries(self):
        for ok, ttl in ((True, 86400), (False, 21600)):
            for age, calls in ((ttl - 1, 0), (ttl, 1)):
                with self.subTest(ok=ok, age=age):
                    write_json(self.cache / "update-check-v1.json", {**self.record, "ok": ok, "checkedAt": self.now - age})
                    with patch.object(helper.time, "time", return_value=self.now), patch.object(helper, "fetch_manifest", return_value=({"version": "v1.4.0"}, 100)) as fetch:
                        helper.automatic(self.args)
                    self.assertEqual(fetch.call_count, calls)
                    if calls:
                        self.assertTrue(fetch.call_args.kwargs["automatic"])
                        record = json.loads((self.cache / "update-check-v1.json").read_text())
                        self.assertEqual(record, {**self.record, "latestVersion": "1.4.0"})

    def test_failed_refresh_retains_prior_notice_and_never_touches_profiles(self):
        write_json(self.cache / "update-check-v1.json", {**self.record, "checkedAt": self.now - 86400})
        profile = self.home / ".hamn/profile"
        profile.mkdir()
        disk = profile / "disk.img"
        disk.write_bytes(b"preserved")
        with patch.object(helper.time, "time", return_value=self.now), patch.object(helper, "fetch_manifest", side_effect=OSError("offline")):
            helper.automatic(self.args)
        record = json.loads((self.cache / "update-check-v1.json").read_text())
        self.assertEqual(record, {**self.record, "ok": False})
        self.assertEqual(disk.read_bytes(), b"preserved")
        self.assertEqual(stat.S_IMODE((self.cache / "update-check-v1.json").stat().st_mode), 0o600)

    def test_single_flight_uses_a_real_cross_process_lock(self):
        context = multiprocessing.get_context("fork")
        ready = context.Event()
        release = context.Event()
        calls = self.home / "fetch-calls"

        def fetch(*args, **kwargs):
            with calls.open("ab") as output: output.write(b"x")
            ready.set()
            if not release.wait(5): raise OSError("test barrier timeout")
            return {"version": "v1.4.0"}, 100

        def run():
            with patch.object(helper, "fetch_manifest", side_effect=fetch):
                helper.automatic(self.args)

        child = context.Process(target=run)
        child.start()
        try:
            self.assertTrue(ready.wait(5), "first checker did not acquire lock")
            with patch.object(helper, "fetch_manifest", side_effect=AssertionError("duplicate fetch")):
                with self.assertRaises(BlockingIOError): helper.automatic(self.args)
            self.assertEqual(calls.read_bytes(), b"x")
        finally:
            release.set()
            child.join(timeout=5)
            if child.is_alive(): child.kill(); child.join(timeout=5)
        self.assertEqual(child.exitcode, 0)


if __name__ == "__main__":
    unittest.main()
