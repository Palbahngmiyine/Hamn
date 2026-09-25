#!/usr/bin/env python3
"""Observe the real headless/worker/installer boundary with controlled transport.

An owned HTTPS endpoint blocks on a socket until the parent observes download
progress. Native curl/TLS execute unchanged; no public network or VM is used.
"""
import contextlib
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import pty
import selectors
import select
import signal
import shutil
import socket
import ssl
import subprocess
import sys
import tarfile
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
HAMN = (ROOT / os.environ.get('HAMN', 'build/hamn')).resolve()
from measure_upgrade_download import certificate


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def error_message(result):
    """The single failure reason in the headless JSON envelope."""
    value = json.loads(result.stdout)
    assert value['ok'] is False, result.stdout
    return value['error']['message']



@contextlib.contextmanager
def controlled_https(root):
    cert, key = certificate(root)
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args): pass
        def do_GET(self):
            if self.path not in ('/host.tar.gz', '/guest.img'):
                self.send_error(404); return
            assert isinstance(self.connection, ssl.SSLSocket)
            with (root / 'transport-requests').open('a') as log:
                log.write(self.path + ' ' + self.connection.version() + '\n')
            if self.path == '/host.tar.gz' and not (root / 'download-observed').exists():
                with socket.socket(socket.AF_UNIX) as channel:
                    channel.settimeout(20)
                    channel.connect(str(root / 'ready.sock'))
                    channel.sendall(b'ready')
                    assert channel.recv(1) == b'!'
            source = root / self.path[1:]
            self.send_response(200)
            self.send_header('Content-Length', str(source.stat().st_size))
            self.send_header('Connection', 'close')
            self.end_headers()
            with source.open('rb') as data:
                shutil.copyfileobj(data, self.wfile, 64 * 1024)
    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(cert, key)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    worker = threading.Thread(target=server.serve_forever)
    worker.start()
    try:
        yield 'https://127.0.0.1:' + str(server.server_address[1]), {
            'CURL_CA_BUNDLE': str(cert), 'SSL_CERT_FILE': str(cert),
            'NO_PROXY': '127.0.0.1', 'no_proxy': '127.0.0.1'}
    finally:
        server.shutdown(); server.server_close(); worker.join(timeout=5)
        assert not worker.is_alive()


def check(terminal):
    with tempfile.TemporaryDirectory(prefix='hamn-update-ux-') as directory, contextlib.ExitStack() as resources:
        work = Path(directory).resolve()
        home, bindir, datadir = work / 'home', work / 'bin', work / 'src'
        home.mkdir()
        version = subprocess.check_output([HAMN, '--version'], text=True).strip().split()[1]
        artifact = work / 'release'
        (artifact / 'bin').mkdir(parents=True)
        shutil.copy2(HAMN, artifact / 'bin/hamn')
        for name in ('scripts', 'packaging'):
            shutil.copytree(ROOT / name, artifact / name)
        (artifact / 'packaging/release/update-manifest-url').write_text('https://example.test/manifest\n')
        # Resolving support from ROOT/scripts would select the shared build/hamn
        # even when HAMN names a frozen binary. Own every installer dependency.
        subprocess.run(['bash', artifact / 'scripts/install-host.sh', artifact / 'bin/hamn', bindir, datadir],
                       env={**os.environ, 'HOME': str(home)}, check=True, capture_output=True)
        original = os.readlink(bindir / 'hamn')
        archive, guest = work / 'host.tar.gz', work / 'guest.img'
        with tarfile.open(archive, 'w:gz') as bundle:
            bundle.add(artifact, arcname='release')
        guest.write_bytes(b'controlled guest fixture\n')
        base_url, tls_env = resources.enter_context(controlled_https(work))
        manifest = {
            'schemaVersion': 2, 'channel': 'stable', 'version': 'v' + version,
            'commit': '1' * 40, 'repository': 'example/hamn',
            'validationMode': 'github-hosted-no-vm',
            'compatibility': {'os': 'darwin', 'architecture': 'arm64', 'minimumMacOS': '13.0'},
            'artifacts': {name: {'url': base_url + '/' + path.name, 'sha256': digest(path)}
                          for name, path in [('host', archive), ('guestImage', guest)]},
        }
        manifest_path = work / 'manifest.json'
        manifest_path.write_text(json.dumps(manifest))
        transport = work / 'transport'
        transport.mkdir()
        env = {**os.environ, **tls_env, 'HOME': str(home),
               'PATH': str(transport) + ':' + os.environ['PATH'],
               # The updater ignores the caller PATH; faults use this seam.
               'HAMN_TEST_UPDATE_TOOL_DIR': str(transport),
               'HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS': '1'}
        listener = socket.socket(socket.AF_UNIX)
        listener.bind(str(work / 'ready.sock'))
        listener.listen(1)
        listener.settimeout(15)
        master, slave = pty.openpty() if terminal else (None, None)
        process = subprocess.Popen([bindir / 'hamn', '--headless', 'system', 'update', '--yes',
                                    '--manifest', manifest_path], env=env,
                                   stdout=subprocess.PIPE, stderr=slave if terminal else subprocess.PIPE)
        # Keep the slave open until the queued output is drained: macOS may
        # discard unread PTY bytes when the final slave descriptor closes.
        received = b''
        try:
            with listener.accept()[0] as blocked:
                reader = master if terminal else process.stderr.fileno()
                with selectors.DefaultSelector() as poll:
                    poll.register(reader, selectors.EVENT_READ)
                    deadline = time.monotonic() + 15
                    while b'Downloading Hamn ' not in received:
                        assert poll.select(max(0, deadline - time.monotonic())), 'progress was buffered until completion'
                        chunk = os.read(reader, 4096)
                        assert chunk, 'stderr ended before download progress'
                        received += chunk
                assert process.poll() is None, 'download fixture must still be blocked'
                (work / 'download-observed').touch()
                blocked.sendall(b'!')
            stdout, stderr = process.communicate(timeout=30)
            assert process.returncode == 0, (received, stderr, stdout)
            if terminal:
                os.set_blocking(master, False)
                while True:
                    try:
                        chunk = os.read(master, 4096)
                        if not chunk:
                            break
                        received += chunk
                    except (BlockingIOError, OSError):
                        break
            else:
                received += stderr
            assert json.loads(stdout)['data']['completed'] is True, stdout
            assert b'Reinstalled Hamn ' + version.encode() + b'.' in received, received
            assert b'Existing VMs were not restarted' in received
            # One heading and one result line; no internal jargon or repeats.
            for jargon in (b'hamn update:', b'verified cache', b'atomically', b'update completed'):
                assert jargon not in received, received
            assert b'.hamn-generations/' not in received and b'file://' not in received
            args = (work / 'transport-requests').read_text()
            assert len(args.splitlines()) == 2 and all(' TLSv1.' in row for row in args.splitlines()), args
            active = os.readlink(bindir / 'hamn')
            assert active != original
            selection = home / '.hamn/cache/guest-image.json'
            saved = selection.read_bytes()
            assert json.loads(saved)['sha256'] == digest(guest)
            command = [bindir / 'hamn', '--headless', 'system', 'update', '--yes',
                       '--manifest', manifest_path]
            def update():
                return subprocess.run(command, env=env, capture_output=True, timeout=30)

            def recover_changed_generation():
                # This invocation started in the interrupted new generation.
                # Recovery restores its predecessor, so continuing the stale
                # frontend would bypass the version/identity-under-lock guard.
                recovered = update()
                assert recovered.returncode != 0, recovered.stdout
                assert not json.loads(recovered.stdout)['ok'], recovered.stdout
                assert b'recovered the previous binary and guest image selection' in recovered.stderr, recovered.stderr
                assert 'managed generation changed while waiting or recovering' in error_message(recovered), recovered.stdout
                assert os.readlink(bindir / 'hamn') == active
                assert selection.read_bytes() == saved
                assert not (home / '.hamn/cache/.hamn-update-transaction').exists()
                return recovered

            # Identical release must not fetch either payload or rewrite state.
            before_calls = (work / 'transport-requests').read_bytes()
            before_mtime = selection.stat().st_mtime_ns
            repeated = update()
            assert repeated.returncode == 0, repeated.stderr
            assert json.loads(repeated.stdout)['data']['completed'] is True
            # A no-op is exactly two human lines.
            assert repeated.stderr == b'Checking for updates...\nHamn ' + version.encode() + b' is up to date.\n', repeated.stderr
            assert (work / 'transport-requests').read_bytes() == before_calls
            assert os.readlink(bindir / 'hamn') == active
            assert selection.read_bytes() == saved and selection.stat().st_mtime_ns == before_mtime
            assert not (home / '.hamn/cache/.hamn-update-transaction').exists()

            # Version equality is insufficient. Missing/corrupt receipt, changed
            # updater files or image selection must take the verified install path.
            for damage in ('missing receipt', 'malformed receipt', 'source changed', 'selection changed'):
                generation = Path(active).parent.parent
                receipt = generation / '.hamn-release.json'
                if damage == 'missing receipt':
                    receipt.unlink()
                elif damage == 'malformed receipt':
                    receipt.write_text('{')
                elif damage == 'source changed':
                    (generation / 'share/hamn/src/packaging/changed.txt').write_text('changed')
                else:
                    selection.write_text('{}')
                calls = len((work / 'transport-requests').read_text().splitlines())
                result = update()
                assert result.returncode == 0, (damage, result.returncode, result.stdout, result.stderr)
                assert b'is up to date' not in result.stderr, damage
                # Both payloads are cached. Only damaged host integrity requires
                # a generation reinstall; guest selection repair preserves it.
                assert len((work / 'transport-requests').read_text().splitlines()) == calls, damage
                assert (os.readlink(bindir / 'hamn') == active) == (damage == 'selection changed'), damage
                active = os.readlink(bindir / 'hamn')
                assert selection.read_bytes() == saved

            # A selected image with corrupted bytes cannot authorize a no-op.
            cached_guest = home / '.hamn/cache' / json.loads(saved)['file']
            cached_guest.write_bytes(b'corrupted')
            result = update()
            assert result.returncode == 0 and json.loads(result.stdout)['data']['status'] == 'repaired', result.stderr
            assert b'is up to date' not in result.stderr
            assert b'Repaired the Hamn ' + version.encode() + b' guest image.' in result.stderr, result.stderr
            assert os.readlink(bindir / 'hamn') == active and selection.read_bytes() == saved
            cached_guest.write_bytes(guest.read_bytes())

            # A new archive with the same version must still be installed.
            (artifact / 'packaging/same-version-change.txt').write_text('new artifact identity')
            with tarfile.open(archive, 'w:gz') as bundle:
                bundle.add(artifact, arcname='release')
            manifest['artifacts']['host']['sha256'] = digest(archive)
            manifest_path.write_text(json.dumps(manifest))
            result = update()
            assert result.returncode == 0 and b'Reinstalled Hamn ' in result.stderr, result.stderr
            assert os.readlink(bindir / 'hamn') != active
            active = os.readlink(bindir / 'hamn')
            # Strict metadata rejection, including malformed known extensions.
            for key, value in [('repository', 7), ('repository', 'bad/name/extra'), ('unexpected', True)]:
                bad = {**manifest, key: value}
                manifest_path.write_text(json.dumps(bad))
                result = subprocess.run([bindir / 'hamn', '--headless', 'system', 'update', '--yes',
                                         '--manifest', manifest_path], env=env, capture_output=True, timeout=15)
                assert result.returncode != 0
                message = error_message(result)
                assert 'is not usable by this Hamn' in message, message
                assert 'https://github.com/Palbahngmiyine/Hamn#install' in message, message
                # The reason is reported once (JSON), not repeated on stderr.
                assert message.encode() not in result.stderr and b'update failed' not in result.stderr, result.stderr
                assert b'retry with hamn --headless system update --yes' not in result.stderr
                assert os.readlink(bindir / 'hamn') == active and selection.read_bytes() == saved
                assert not (home / '.hamn/cache/.hamn-update-transaction').exists()
            for failure in ('version', 'checksum'):
                bad = json.loads(json.dumps(manifest))
                if failure == 'version':
                    bad['version'] = 'v999.0.0'
                else:
                    bad['artifacts']['guestImage']['sha256'] = '0' * 64
                manifest_path.write_text(json.dumps(bad))
                result = subprocess.run([bindir / 'hamn', '--headless', 'system', 'update', '--yes',
                                         '--manifest', manifest_path], env=env, capture_output=True, timeout=15)
                assert result.returncode != 0
                message = error_message(result)
                expected = ('host binary version does not match' if failure == 'version'
                            else 'could not obtain the guest image: artifact size or SHA-256 mismatch')
                assert expected in message, message
                assert b'Updated Hamn' not in result.stderr and b'Reinstalled Hamn' not in result.stderr
                assert os.readlink(bindir / 'hamn') == active and selection.read_bytes() == saved
            # Reinstalling through bootstrap must preserve the managed binary,
            # including SIGKILL recovery and a later failed installation attempt.
            if not terminal:
                (artifact / 'packaging/same-version-change.txt').write_text('next candidate')
                with tarfile.open(archive, 'w:gz') as bundle:
                    bundle.add(artifact, arcname='release')
                interrupted_manifest = json.loads(json.dumps(manifest))
                interrupted_manifest['artifacts']['host']['sha256'] = digest(archive)
                manifest_path.write_text(json.dumps(interrupted_manifest))
                for termination in (signal.SIGTERM, signal.SIGKILL):
                    ready, release = work / f'ready-{termination}', work / f'release-{termination}'
                    os.mkfifo(ready)
                    os.mkfifo(release)
                    ready_fd = os.open(ready, os.O_RDWR | os.O_NONBLOCK)
                    child = subprocess.Popen(['bash', artifact / 'scripts/update-host.sh', '--bootstrap',
                                              '--bindir', bindir, '--datadir', datadir,
                                              '--manifest', manifest_path],
                                             env={**env, 'HAMN_TEST_UPDATE_AFTER_HOST_INSTALL_READY_FIFO': str(ready),
                                                  'HAMN_TEST_UPDATE_AFTER_HOST_INSTALL_RELEASE_FIFO': str(release)},
                                             stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                    try:
                        if not select.select([ready_fd], [], [], 30)[0]:
                            if child.poll() is None:
                                child.kill()
                            raise AssertionError(('bootstrap did not reach host cutover', child.communicate(timeout=10)))
                        assert os.read(ready_fd, 32) == b'ready\n'
                        assert os.readlink(bindir / 'hamn') != active
                        child.send_signal(termination)
                        _, diagnostic = child.communicate(timeout=15)
                        journal = home / '.hamn/cache/.hamn-update-transaction'
                        if termination == signal.SIGTERM:
                            assert child.returncode == 143, diagnostic
                            assert os.readlink(bindir / 'hamn') == active
                            assert not journal.exists()
                        else:
                            assert child.returncode == -signal.SIGKILL
                            assert journal.exists()
                            assert (journal / 'old-target').read_text().strip() == active
                            recover_changed_generation()
                        assert selection.read_bytes() == saved
                    finally:
                        os.close(ready_fd)
                        if child.poll() is None:
                            child.kill()
                            child.communicate(timeout=10)

            # Logging failure must not bypass rollback after installer failure.
            installer_source = (artifact / 'scripts/install-host.sh').read_text()
            (artifact / 'scripts/install-host.sh').write_text('#!/bin/bash\necho installer-failed >&2\nexit 77\n')
            with tarfile.open(archive, 'w:gz') as bundle:
                bundle.add(artifact, arcname='release')
            failed_install = json.loads(json.dumps(manifest))
            failed_install['artifacts']['host']['sha256'] = digest(archive)
            manifest_path.write_text(json.dumps(failed_install))
            cat = transport / 'cat'
            cat.write_text('#!/bin/bash\ncase "$1" in */host-install.log) exit 73;; esac\nexec /bin/cat "$@"\n')
            cat.chmod(0o755)
            result = subprocess.run([bindir / 'hamn', '--headless', 'system', 'update', '--yes',
                                     '--manifest', manifest_path], env=env, capture_output=True, timeout=15)
            assert result.returncode != 0
            assert 'host install failed; prior binary and guest image selection were restored' in error_message(result), (result.stdout, result.stderr)
            assert os.readlink(bindir / 'hamn') == active and selection.read_bytes() == saved
            assert not (home / '.hamn/cache/.hamn-update-transaction').exists()
            manifest_path.write_text(json.dumps(manifest))
            repeated = update()
            assert repeated.returncode == 0 and b'is up to date' in repeated.stderr, repeated.stderr
            assert os.readlink(bindir / 'hamn') == active and selection.read_bytes() == saved
            # Receipt publication is part of the transaction. Inject an existing
            # receipt and a rollback rename failure; retry must recover the old
            # generation and its valid receipt before considering a no-op.
            if not terminal:
                (artifact / 'scripts/install-host.sh').write_text(installer_source +
                    '\ninstalled=$(readlink "$BINDIR/hamn")\n' +
                    ': >"${installed%/bin/hamn}/.hamn-release.json"\n')
                with tarfile.open(archive, 'w:gz') as bundle:
                    bundle.add(artifact, arcname='release')
                receipt_failure = json.loads(json.dumps(manifest))
                receipt_failure['artifacts']['host']['sha256'] = digest(archive)
                manifest_path.write_text(json.dumps(receipt_failure))
                move = transport / 'mv'
                move.write_text('#!/bin/bash\nfor arg in "$@"; do\n' +
                                'case "$arg" in */.hamn-update-rollback.*/hamn) exit 74;; esac\n' +
                                'done\nexec /bin/mv "$@"\n')
                move.chmod(0o755)
                result = update()
                assert result.returncode != 0, result.stderr
                assert 'release receipt failed and recovery could not be applied' in error_message(result), result.stdout
                assert 'were restored' not in error_message(result) and b'were restored' not in result.stderr
                assert os.readlink(bindir / 'hamn') != active
                assert selection.read_bytes() == saved
                assert (home / '.hamn/cache/.hamn-update-transaction').is_dir()
                move.unlink()
                manifest_path.write_text(json.dumps(manifest))
                recover_changed_generation()
                recovered = update()
                assert recovered.returncode == 0, recovered.stderr
                assert b'is up to date' in recovered.stderr
                assert os.readlink(bindir / 'hamn') == active and selection.read_bytes() == saved
                assert not (home / '.hamn/cache/.hamn-update-transaction').exists()
                # If both completed/recovered journal retirement fail, preserve
                # the original manifest in the remaining recovery instruction.
                (artifact / 'scripts/install-host.sh').write_text(installer_source)
                with tarfile.open(archive, 'w:gz') as bundle:
                    bundle.add(artifact, arcname='release')
                retirement_failure = json.loads(json.dumps(manifest))
                retirement_failure['artifacts']['host']['sha256'] = digest(archive)
                manifest_path.write_text(json.dumps(retirement_failure))
                move.write_text('#!/bin/bash\nfor arg in "$@"; do\n' +
                                'case "$arg" in */.hamn-update-completed.*|*/.hamn-update-recovered.*) exit 75;; esac\n' +
                                'done\nexec /bin/mv "$@"\n')
                move.chmod(0o755)
                result = update()
                assert result.returncode != 0, result.stderr
                assert 'could not clear its recovery journal; retry the same command with all original options (including --manifest)' in error_message(result), result.stdout
                assert b'run hamn --headless system update --yes again' not in result.stdout + result.stderr
                assert os.readlink(bindir / 'hamn') == active and selection.read_bytes() == saved
                assert (home / '.hamn/cache/.hamn-update-transaction').is_dir()
                move.unlink()
                manifest_path.write_text(json.dumps(manifest))
                recovered = update()
                assert recovered.returncode == 0 and b'is up to date' in recovered.stderr, recovered.stderr
                assert os.readlink(bindir / 'hamn') == active and selection.read_bytes() == saved
                assert not (home / '.hamn/cache/.hamn-update-transaction').exists()
            print(f'PASS: {"PTY" if terminal else "redirected"} progress before completion, JSON, version summary, schema rejection, no-op identity and state preservation')
        finally:
            if process.poll() is None:
                process.terminate()
                process.communicate(timeout=10)
            listener.close()
            if master is not None:
                os.close(master)
                os.close(slave)


if __name__ == '__main__':
    check(False)
    check(True)
