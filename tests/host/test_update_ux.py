#!/usr/bin/env python3
"""Observe the real headless/worker/installer boundary with controlled transport.

The curl fixture blocks on a socket until the parent has observed the download
stage. No real VM or network is used; installation is isolated under a temp HOME.
"""
import hashlib
import json
import os
from pathlib import Path
import pty
import selectors
import select
import signal
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
HAMN = (ROOT / os.environ.get('HAMN', 'build/hamn')).resolve()


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def check(terminal):
    with tempfile.TemporaryDirectory(prefix='hamn-update-ux-') as directory:
        work = Path(directory).resolve()
        home, bindir, datadir = work / 'home', work / 'bin', work / 'src'
        home.mkdir()
        version = subprocess.check_output([HAMN, '--version'], text=True).strip().split()[1]
        subprocess.run(['bash', ROOT / 'scripts/install-host.sh', HAMN, bindir, datadir],
                       check=True, capture_output=True)
        original = os.readlink(bindir / 'hamn')
        artifact = work / 'release'
        (artifact / 'bin').mkdir(parents=True)
        shutil.copy2(HAMN, artifact / 'bin/hamn')
        for name in ('scripts', 'packaging'):
            shutil.copytree(ROOT / name, artifact / name)
        (artifact / 'packaging/release/update-manifest-url').write_text('https://example.test/manifest\n')
        archive, guest = work / 'host.tar.gz', work / 'guest.img'
        with tarfile.open(archive, 'w:gz') as bundle:
            bundle.add(artifact, arcname='release')
        guest.write_bytes(b'controlled guest fixture\n')
        manifest = {
            'schemaVersion': 2, 'channel': 'stable', 'version': 'v' + version,
            'commit': '1' * 40, 'repository': 'example/hamn',
            'validationMode': 'github-hosted-no-vm',
            'compatibility': {'os': 'darwin', 'architecture': 'arm64', 'minimumMacOS': '13.0'},
            'artifacts': {name: {'url': 'https://example.test/' + path.name, 'sha256': digest(path)}
                          for name, path in [('host', archive), ('guestImage', guest)]},
        }
        manifest_path = work / 'manifest.json'
        manifest_path.write_text(json.dumps(manifest))
        transport = work / 'transport'
        transport.mkdir()
        curl = transport / 'curl'
        curl.write_text(f'#!{sys.executable}\n' + '''import os, pathlib, socket, sys
root = pathlib.Path(os.environ['UX_WORK'])
args = sys.argv[1:]
with (root / 'curl-args').open('a') as log:
    log.write(' '.join(args) + '\\n')
if args[-1].endswith('host.tar.gz') and not (root / 'download-observed').exists():
    with socket.socket(socket.AF_UNIX) as channel:
        channel.connect(str(root / 'ready.sock'))
        channel.sendall(b'ready')
        assert channel.recv(1) == b'!'
source = root / args[-1].rsplit('/', 1)[-1]
pathlib.Path(args[args.index('-o') + 1]).write_bytes(source.read_bytes())
''')
        curl.chmod(0o755)
        env = {**os.environ, 'HOME': str(home), 'PATH': str(transport) + ':' + os.environ['PATH'],
               'HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS': '1', 'UX_WORK': str(work)}
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
                    while b'Downloading host archive' not in received:
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
            assert b'Updated Hamn:' in received and version.encode() in received
            assert b'Existing VMs were not restarted' in received
            assert b'.hamn-generations/' not in received and b'file://' not in received
            args = (work / 'curl-args').read_text()
            assert ('--progress-bar' in args) == terminal, args
            assert ('--silent' in args) != terminal, args
            active = os.readlink(bindir / 'hamn')
            assert active != original
            selection = home / '.hamn/cache/guest-image.json'
            saved = selection.read_bytes()
            assert json.loads(saved)['sha256'] == digest(guest)
            command = [bindir / 'hamn', '--headless', 'system', 'update', '--yes',
                       '--manifest', manifest_path]
            def update():
                return subprocess.run(command, env=env, capture_output=True, timeout=30)

            # Identical release must not fetch either payload or rewrite state.
            before_calls = (work / 'curl-args').read_bytes()
            before_mtime = selection.stat().st_mtime_ns
            repeated = update()
            assert repeated.returncode == 0, repeated.stderr
            assert json.loads(repeated.stdout)['data']['completed'] is True
            assert b'Unchanged Hamn' in repeated.stderr and b'Updated Hamn:' not in repeated.stderr
            assert (work / 'curl-args').read_bytes() == before_calls
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
                calls = len((work / 'curl-args').read_text().splitlines())
                result = update()
                assert result.returncode == 0, (damage, result.stderr)
                assert b'Unchanged Hamn' not in result.stderr, damage
                assert len((work / 'curl-args').read_text().splitlines()) == calls + 2, damage
                assert os.readlink(bindir / 'hamn') != active, damage
                active = os.readlink(bindir / 'hamn')
                assert selection.read_bytes() == saved

            # A selected image with corrupted bytes cannot authorize a no-op.
            cached_guest = home / '.hamn/cache' / json.loads(saved)['file']
            cached_guest.write_bytes(b'corrupted')
            result = update()
            assert result.returncode != 0 and b'cached guest image SHA-256 mismatch' in result.stderr
            assert b'Unchanged Hamn' not in result.stderr
            assert os.readlink(bindir / 'hamn') == active and selection.read_bytes() == saved
            cached_guest.write_bytes(guest.read_bytes())

            # A new archive with the same version must still be installed.
            (artifact / 'packaging/same-version-change.txt').write_text('new artifact identity')
            with tarfile.open(archive, 'w:gz') as bundle:
                bundle.add(artifact, arcname='release')
            manifest['artifacts']['host']['sha256'] = digest(archive)
            manifest_path.write_text(json.dumps(manifest))
            result = update()
            assert result.returncode == 0 and b'Updated Hamn:' in result.stderr, result.stderr
            assert os.readlink(bindir / 'hamn') != active
            active = os.readlink(bindir / 'hamn')
            # Strict metadata rejection, including malformed known extensions.
            for key, value in [('repository', 7), ('repository', 'bad/name/extra'), ('unexpected', True)]:
                bad = {**manifest, key: value}
                manifest_path.write_text(json.dumps(bad))
                result = subprocess.run([bindir / 'hamn', '--headless', 'system', 'update', '--yes',
                                         '--manifest', manifest_path], env=env, capture_output=True, timeout=15)
                assert result.returncode != 0 and not json.loads(result.stdout)['ok']
                assert b'No new release was installed' in result.stderr
                assert b'retry the same command with all original options (including --manifest, if supplied)' in result.stderr
                assert b'retry with hamn --headless system update --yes' not in result.stderr
                assert b'https://github.com/Palbahngmiyine/Hamn#install' in result.stderr
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
                assert result.returncode != 0 and not json.loads(result.stdout)['ok']
                expected = b'version does not match' if failure == 'version' else b'guest image SHA-256 mismatch'
                assert expected in result.stderr, result.stderr
                assert b'Updated Hamn:' not in result.stderr
                assert os.readlink(bindir / 'hamn') == active and selection.read_bytes() == saved
            # Reinstalling through bootstrap must preserve the managed binary,
            # including SIGKILL followed by a second failed installation attempt.
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
                    child = subprocess.Popen(['bash', ROOT / 'scripts/update-host.sh', '--bootstrap',
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
            assert result.returncode != 0 and not json.loads(result.stdout)['ok']
            assert b'host install failed; prior binary and guest image selection were restored' in result.stderr
            assert os.readlink(bindir / 'hamn') == active and selection.read_bytes() == saved
            assert not (home / '.hamn/cache/.hamn-update-transaction').exists()
            if not terminal:
                assert b'recovered the previous binary and guest image selection' in result.stderr
            manifest_path.write_text(json.dumps(manifest))
            repeated = update()
            assert repeated.returncode == 0 and b'Unchanged Hamn' in repeated.stderr, repeated.stderr
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
                assert b'release receipt failed and recovery could not be applied' in result.stderr
                assert b'were restored' not in result.stderr
                assert os.readlink(bindir / 'hamn') != active
                assert selection.read_bytes() == saved
                assert (home / '.hamn/cache/.hamn-update-transaction').is_dir()
                move.unlink()
                manifest_path.write_text(json.dumps(manifest))
                recovered = update()
                assert recovered.returncode == 0, recovered.stderr
                assert b'recovered the previous binary and guest image selection' in recovered.stderr
                assert b'Unchanged Hamn' in recovered.stderr
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
                assert b'could not clear its recovery journal; retry the same command with all original options (including --manifest)' in result.stderr
                assert b'run hamn --headless system update --yes again' not in result.stderr
                assert os.readlink(bindir / 'hamn') == active and selection.read_bytes() == saved
                assert (home / '.hamn/cache/.hamn-update-transaction').is_dir()
                move.unlink()
                manifest_path.write_text(json.dumps(manifest))
                recovered = update()
                assert recovered.returncode == 0 and b'Unchanged Hamn' in recovered.stderr, recovered.stderr
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
