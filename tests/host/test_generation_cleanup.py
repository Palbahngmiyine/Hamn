#!/usr/bin/env python3
"""Real installer/process regressions; all writes are inside a disposable root."""
import os
import hashlib
from pathlib import Path
import shutil
import subprocess
import tempfile

repo = Path.cwd()
with tempfile.TemporaryDirectory(prefix='hamn-generation-test-') as work:
    root = Path(work).resolve()
    bindir, datadir = root / 'bin', root / 'data'
    home = root / 'home'
    home.mkdir()
    env = dict(os.environ, HOME=str(home))
    binary = repo / os.environ.get('HAMN', 'build/hamn')

    def install(source=binary):
        subprocess.run(['bash', 'scripts/install-host.sh', str(source), str(bindir), str(datadir)],
                       env=env, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
        return Path(os.readlink(bindir / 'hamn')).parent.parent

    def collect(previous=''):
        subprocess.run(['bash', '-c', 'ROOT=$PWD; source scripts/install-support.sh; source scripts/install-transaction.sh; '
                        'install_support prune "$BINDIR" "$DATADIR" "$1" "$PWD"',
                        'collect', str(previous)], env=dict(env, BINDIR=str(bindir), DATADIR=str(datadir)),
                       check=True, timeout=60, stdout=subprocess.PIPE)

    # A foreign/symlinked transaction lock must fail before it is opened.
    bad_bin, bad_data = root / 'bad-bin', root / 'bad-data'
    bad_bin.mkdir()
    sentinel = root / 'lock-sentinel'
    sentinel.write_text('keep')
    (bad_bin / '.hamn-transaction.lock').symlink_to(sentinel)
    rejected = subprocess.run(['bash', 'scripts/install-host.sh', str(binary), str(bad_bin), str(bad_data)],
                              env=env, capture_output=True, timeout=30)
    assert rejected.returncode != 0 and sentinel.read_text() == 'keep'
    assert not bad_data.exists()

    first = install()
    second = install()
    third = install()
    assert not first.exists(), 'repeated installs accumulated obsolete generations'
    assert second.exists() and third.exists(), 'active/predecessor removed'
    assert len(list((datadir / '.hamn-generations').iterdir())) == 2
    collect(third / 'bin/hamn')
    assert second.exists(), 'unchanged release collection removed predecessor'

    # Unknown directories, symlinks, and mismatched receipts are never adopted.
    foreign = datadir / '.hamn-generations' / ('a' * 64 + '-ABC123')
    foreign.mkdir()
    (foreign / 'sentinel').write_text('foreign')
    outside = root / 'outside'
    outside.mkdir()
    (outside / 'sentinel').write_text('keep')
    link = datadir / '.hamn-generations' / ('b' * 64 + '-ABC123')
    link.symlink_to(outside, target_is_directory=True)
    fourth = install()
    assert foreign.exists() and link.is_symlink() and (outside / 'sentinel').read_text() == 'keep'

    # Recovery metadata preserves every generation until it can be interpreted
    # safely; a different HOME is remembered by the generation's recovery root.
    cache = home / '.hamn/cache'
    cache.mkdir(parents=True, mode=0o755)
    journal = cache / '.hamn-update-transaction'
    journal.mkdir(mode=0o700)
    fifth = install()
    assert third.exists()
    journal.rmdir()
    other_cache = root / 'other-home/cache'
    other_cache.mkdir(parents=True)
    other_journal = other_cache / '.hamn-update-transaction'
    other_journal.mkdir()
    (third / ('.hamn-recovery-root-' + hashlib.sha256(str(other_cache).encode()).hexdigest())).write_text(str(other_cache))
    sixth = install()
    assert third.exists() and not fourth.exists()
    for blocked in (other_cache.parent, other_cache):
        blocked.chmod(0)
        try:
            install()
            assert third.exists(), 'inaccessible recovery state was treated as absent'
        finally:
            blocked.chmod(0o755)
    other_journal.rmdir()
    seventh = install()
    assert not third.exists()

    # The same transaction lock spans updater recovery and installer publication.
    holder = subprocess.Popen(['bash', '-c',
        'ROOT=$PWD; source scripts/install-support.sh; source scripts/install-transaction.sh; echo ready; read -r release'],
        env=dict(env, BINDIR=str(bindir), DATADIR=str(datadir)),
        stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    waiting = None
    try:
        import select
        assert select.select([holder.stdout], [], [], 5)[0]
        assert holder.stdout.readline() == b'ready\n'
        waiting = subprocess.Popen(['bash', 'scripts/install-host.sh', str(binary), str(bindir), str(datadir)],
                                   env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        assert not select.select([waiting.stdout], [], [], 1)[0]
        assert waiting.poll() is None
        assert Path(os.readlink(bindir / 'hamn')).parent.parent == seventh
        # SIGKILL releases the lock; no stale PID/lock deletion is needed.
        holder.kill()
        holder.wait(timeout=5)
        waiting.communicate(timeout=60)
        assert waiting.returncode == 0
    finally:
        for child in (holder, waiting):
            if child is not None and child.poll() is None:
                child.kill()
                child.wait(timeout=5)
        holder.stdin.close()
        holder.stdout.close()

    # A real native executable stays alive across more than two installations.
    src = root / 'wait.c'
    src.write_text('#include <stdio.h>\n#include <unistd.h>\nint main(void) { puts("ready"); fflush(stdout); for (;;) pause(); }\n')
    sleeper = root / 'wait'
    subprocess.run(['cc', str(src), '-o', str(sleeper)], check=True)
    running = install(sleeper)
    process = subprocess.Popen([str(running / 'bin/hamn')], stdout=subprocess.PIPE)
    try:
        import select
        assert select.select([process.stdout], [], [], 5)[0]
        assert process.stdout.readline() == b'ready\n'
        install()
        active = install()
        assert running.exists() and process.poll() is None
    finally:
        process.terminate()
        process.wait(timeout=5)
        process.stdout.close()
    install()
    assert not running.exists(), 'exited executable was never collected'

    # An owned, unreferenced generation is collected without the retired
    # pre-0.1.2 `.hamn-retention` opt-in marker (never written any more).
    current = Path(os.readlink(bindir / 'hamn')).parent.parent
    assert not (current / '.hamn-retention').exists()
    unmarked = current.with_name(current.name[:64] + '-UNMARK')
    shutil.copytree(current, unmarked)
    collect()
    assert not unmarked.exists()

    saved_previous = (current / '.hamn-previous-target').read_bytes()
    (current / '.hamn-previous-target').write_text('')
    invalid = subprocess.run(['bash', '-c', 'ROOT=$PWD; source scripts/install-support.sh; source scripts/install-transaction.sh; '
                             'install_support prune "$BINDIR" "$DATADIR" "" "$PWD"'],
                            env=dict(env, BINDIR=str(bindir), DATADIR=str(datadir)),
                            capture_output=True, timeout=60)
    assert invalid.returncode != 0 and b'invalid predecessor reference' in invalid.stderr
    (current / '.hamn-previous-target').write_bytes(saved_previous)

    # An unavailable process scan must preserve a collectible generation.
    candidate = install()
    spare = candidate.with_name(candidate.name[:64] + '-FAULT1')
    shutil.copytree(candidate, spare)
    profile = root / 'deny-scanner.sb'
    profile.write_text('(version 1)(allow default)(deny process-exec (literal "/usr/sbin/lsof"))\n')
    failed = subprocess.run(['/usr/bin/sandbox-exec', '-f', str(profile), 'bash', '-c',
                    'ROOT=$PWD; source scripts/install-support.sh; source scripts/install-transaction.sh; '
                    'install_support prune "$BINDIR" "$DATADIR" "" "$PWD"'],
                   env=dict(env, BINDIR=str(bindir), DATADIR=str(datadir)), capture_output=True, timeout=60)
    assert failed.returncode != 0
    assert spare.exists()
    collect()
    assert not spare.exists()

    # Interrupted retirement is retryable even after the binary has gone.
    old = Path((candidate / '.hamn-previous-target').read_text().strip()).parent.parent
    retired = old.with_name('.retired-' + old.name)
    old.rename(retired)
    shutil.rmtree(retired / 'bin')
    collect()
    assert not retired.exists()

print('OK: bounded generations, ownership, recovery roots, running executable and retired retry')
