#!/usr/bin/env python3
"""Exercise the real pre-helper acquisition program, without a shared build.

Only the fixture curl origin is redirected to loopback after checking HTTPS
restrictions. The full, unmodified installer has a separate system-tools gate.
"""
import fcntl
import hashlib
import http.server
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
TEMPLATE = (ROOT / "packaging/release/install.sh.in").read_text()
PROGRAM = TEMPLATE.split("<<'HAMN_BOOTSTRAP_ACQUIRE'\n", 1)[1].split("\nHAMN_BOOTSTRAP_ACQUIRE", 1)[0]


def eventually(predicate):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.01)
    raise AssertionError("observable fixture event did not arrive")


class BootstrapAcquisition(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="hamn-bootstrap-")
        self.root = Path(self.tmp.name).resolve()
        self.home = self.root / "home"
        self.home.mkdir(mode=0o700)
        self.payload = bytes(range(256)) * 1024
        self.key = hashlib.sha256(self.payload).hexdigest()
        self.store = self.home / ".hamn/cache/downloads"
        self.final = self.store / (self.key + ".artifact")
        self.partial = self.store / ("." + self.key + ".partial")
        self.lock = self.store / ("." + self.key + ".lock")
        self.mode = "normal"
        self.requests = []
        self.release = threading.Event()
        self.children = []
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                requested = self.headers.get("Range")
                outer.requests.append((requested, self.headers.get("If-Range")))
                if outer.mode == "disconnect":
                    self.close_connection = True
                    return
                offset = int(requested.split("=")[1].split("-")[0]) if requested else 0
                ignored = outer.mode == "ignore" and requested
                response = 206 if requested and not ignored else 200
                if outer.mode == "reject" and requested:
                    self.send_error(416)
                    return
                body = outer.payload[0 if ignored else offset:]
                self.send_response(response)
                if outer.mode != "oversize":
                    self.send_header("Content-Length", str(len(body)))
                self.send_header("ETag", '"bootstrap-fixture"')
                if response == 206:
                    start = offset + (1 if outer.mode == "bad-range" else 0)
                    self.send_header("Content-Range", f"bytes {start}-{len(outer.payload)-1}/{len(outer.payload)}")
                self.end_headers()
                try:
                    if outer.mode == "pause":
                        self.wfile.write(body[:65536]); self.wfile.flush()
                        outer.release.wait(15)
                        body = body[65536:]
                    elif outer.mode == "interrupt":
                        body = body[:1024]
                    elif outer.mode == "oversize":
                        body += b"overflow"
                    self.wfile.write(body); self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass
                self.close_connection = True

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        transport = self.root / "curl"
        transport.write_text(f"#!{sys.executable}\n" + f'''import os, sys
args = sys.argv[1:]
assert args[args.index('--proto') + 1] == '=https'
assert args[args.index('--proto-redir') + 1] == '=https'
assert '--tlsv1.2' in args and '--max-time' in args and '--max-filesize' in args
assert args[-1] == 'https://bootstrap.test/host'
for key in ('--proto', '--proto-redir'):
    args[args.index(key) + 1] = '=http'
args[-1] = 'http://127.0.0.1:{self.server.server_port}/host'
os.execv('/usr/bin/curl', ['curl'] + args)
''')
        transport.chmod(0o700)
        self.program = self.root / "acquire.zsh"
        self.program.write_text(PROGRAM.replace("exec /usr/bin/curl", "exec " + shlex.quote(str(transport))))

    def tearDown(self):
        self.release.set()
        for process in self.children:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
            process.communicate(timeout=10)
        self.server.shutdown(); self.server.server_close(); self.thread.join()
        self.tmp.cleanup()

    def spawn(self, key=None, size=None):
        scratch = self.root / ("scratch-" + str(len(self.children)))
        scratch.mkdir(mode=0o700)
        args = ["/bin/zsh", "-f", self.program, self.home, "https://bootstrap.test/host",
                key or self.key, str(len(self.payload) if size is None else size), scratch, "0"]
        process = subprocess.Popen(args, env={"HOME": str(self.home), "LC_ALL": "C",
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin"}, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            start_new_session=True)
        self.children.append(process)
        return process

    def finish(self, process, success=True):
        out, err = process.communicate(timeout=20)
        self.assertEqual(process.returncode == 0, success, (process.returncode, out, err))
        return out, err

    def acquire(self, success=True, **args):
        return self.finish(self.spawn(**args), success)

    def wait_for_partial(self, process):
        def ready():
            if process.poll() is not None:
                self.fail("worker exited before partial: " + repr(process.communicate()))
            return self.partial.exists() and self.partial.stat().st_size > 0
        eventually(ready)

    def test_cold_warm_corruption_and_private_publication(self):
        self.acquire()
        self.assertEqual(self.final.read_bytes(), self.payload)
        self.assertEqual(self.final.stat().st_mode & 0o7777, 0o600)
        self.acquire()
        self.assertEqual(len(self.requests), 1)
        self.final.write_bytes(b"corrupt")
        self.acquire()
        self.assertEqual(self.final.read_bytes(), self.payload)
        self.assertEqual(len(self.requests), 2)

    def test_interrupted_transfer_resumes_only_missing_bytes_with_validator(self):
        self.mode = "interrupt"
        self.acquire(success=False)
        self.assertEqual(self.partial.read_bytes(), self.payload[:1024])
        self.assertFalse(self.final.exists())
        self.mode = "normal"
        self.acquire()
        self.assertEqual(self.requests, [(None, None), ("bytes=1024-", '"bootstrap-fixture"')])
        self.assertEqual(self.final.read_bytes(), self.payload)

    def test_connection_failure_during_resume_preserves_prior_prefix(self):
        self.mode = "interrupt"
        self.acquire(success=False)
        self.mode = "disconnect"
        self.acquire(success=False)
        self.assertEqual(self.partial.read_bytes(), self.payload[:1024])
        self.assertEqual(len(self.requests), 2, "a transport failure must not trigger a full retry")
        self.mode = "normal"
        self.acquire()
        self.assertEqual(self.requests[-1], ("bytes=1024-", '"bootstrap-fixture"'))
        self.assertEqual(self.final.read_bytes(), self.payload)

    def test_ignored_malformed_or_rejected_range_retries_once(self):
        for mode in ("ignore", "bad-range", "reject"):
            with self.subTest(mode=mode):
                if self.final.exists():
                    self.final.unlink()
                self.requests.clear()
                self.mode = "interrupt"
                self.acquire(success=False)
                self.mode = mode
                self.acquire()
                self.assertEqual([row[0] for row in self.requests], [None, "bytes=1024-", None])
                self.assertEqual(self.final.read_bytes(), self.payload)

    def test_integrity_and_unbounded_response_fail_closed(self):
        self.acquire(success=False, key="0" * 64)
        self.assertFalse((self.store / ("." + "0" * 64 + ".partial")).exists())
        self.mode = "oversize"
        self.acquire(success=False)
        self.assertFalse(self.final.exists())
        self.assertFalse(self.partial.exists())

    def test_shared_metadata_requires_exact_versioned_lines(self):
        self.acquire()
        valid = f'hamn-download 1\n{self.key}\n{len(self.payload)}\n"bootstrap-fixture"\n'
        for invalid in (valid + "extra", valid + "\n", valid.rstrip("\n"),
                        valid.replace("download 1", "download 2"),
                        valid.replace(str(len(self.payload)), "0" + str(len(self.payload))),
                        valid.replace("bootstrap-fixture", "bad\rvalue"),
                        valid.replace("bootstrap-fixture", "x" * 1025)):
            with self.subTest(invalid=repr(invalid[-80:])):
                self.final.unlink()
                self.partial.write_bytes(self.payload[:1024]); self.partial.chmod(0o600)
                metadata = self.store / ("." + self.key + ".validator")
                metadata.write_text(invalid); metadata.chmod(0o600)
                self.acquire()
                self.assertIsNone(self.requests[-1][0], "malformed metadata was reused")

    def test_unsafe_paths_and_multilinks_are_preserved_without_network(self):
        self.acquire()
        before = len(self.requests)
        saved = self.root / "saved"
        self.final.rename(saved)
        self.final.symlink_to(saved)
        self.acquire(success=False)
        self.final.unlink()
        os.link(saved, self.final)
        self.acquire(success=False)
        self.final.unlink()
        saved.rename(self.final)
        self.final.chmod(0o666)
        self.acquire(success=False)
        self.assertEqual(self.final.read_bytes(), self.payload)
        self.assertEqual(len(self.requests), before)

    def test_unsafe_parent_and_lock_are_rejected_before_transfer(self):
        outside = self.root / "outside"
        outside.mkdir(mode=0o700)
        (self.home / ".hamn").symlink_to(outside)
        self.acquire(success=False)
        self.assertEqual(list(outside.iterdir()), [])
        (self.home / ".hamn").unlink()
        self.acquire()
        self.final.unlink()
        before = len(self.requests)
        self.lock.chmod(0o644)
        self.acquire(success=False)
        self.assertEqual(len(self.requests), before)
        self.assertEqual(self.lock.stat().st_mode & 0o7777, 0o644)

    def test_failed_publication_reuses_complete_partial_without_another_request(self):
        mover = self.root / "move"
        mover.write_text('#!/bin/bash\ncase "$2" in *.artifact) exit 75;; esac\nexec /bin/mv "$@"\n')
        mover.chmod(0o700)
        production = self.program.read_text()
        self.program.write_text(production.replace('/bin/mv "$partial" "$final"',
            shlex.quote(str(mover)) + ' "$partial" "$final"'))
        self.acquire(success=False)
        self.assertFalse(self.final.exists())
        self.assertEqual(self.partial.read_bytes(), self.payload)
        self.program.write_text(production)
        self.acquire()
        self.assertEqual(self.final.read_bytes(), self.payload)
        self.assertEqual(len(self.requests), 1)

    def test_parallel_installers_and_native_flock_share_one_transfer(self):
        self.mode = "pause"
        first = self.spawn()
        self.wait_for_partial(first)
        with self.lock.open("r+") as held:
            with self.assertRaises(BlockingIOError):
                fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
        others = [self.spawn(), self.spawn()]
        self.release.set()
        for process in [first, *others]:
            self.finish(process)
        self.assertEqual(len(self.requests), 1)
        # Reverse direction, including after the shell's publication/exit.
        self.final.unlink()
        with self.lock.open("r+") as held:
            fcntl.flock(held, fcntl.LOCK_EX)
            blocked = subprocess.run(["/bin/zsh", "-fc", 'zmodload zsh/system; zsystem flock -t 0 "$1"',
                "probe", self.lock], capture_output=True, timeout=5)
            self.assertNotEqual(blocked.returncode, 0)
            child = self.spawn()
            fcntl.flock(held, fcntl.LOCK_UN)
        self.finish(child)
        self.assertEqual(len(self.requests), 2)

    def test_killed_lock_owner_keeps_bounded_partial_and_next_call_recovers(self):
        self.mode = "pause"
        process = self.spawn()
        self.wait_for_partial(process)
        os.killpg(process.pid, signal.SIGKILL)
        self.finish(process, success=False)
        preserved = self.partial.stat().st_size
        self.assertGreater(preserved, 0)
        self.assertLessEqual(preserved, 65536)
        self.assertEqual(self.partial.read_bytes(), self.payload[:preserved])
        self.release.set()
        self.mode = "normal"
        self.acquire()
        self.assertEqual(self.requests[-1][0], f"bytes={preserved}-")
        self.assertEqual(self.final.read_bytes(), self.payload)


if __name__ == "__main__":
    unittest.main()
