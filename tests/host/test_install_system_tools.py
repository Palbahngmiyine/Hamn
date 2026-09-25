#!/usr/bin/env python3
"""Install/update in an isolated HOME with only OS commands + shipped hamn executable.
Python drives fixtures outside the restricted child; it is forbidden inside it.
No package manager, compiler, interpreter, or developer-tool shim may execute.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import stat
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[2]
HAMN = (ROOT / os.environ.get('HAMN', 'build/hamn')).resolve()
TOOLS = ['bash', 'sh', 'zsh', 'env', 'curl', 'openssl', 'tar', 'bsdtar', 'chmod', 'mkdir',
         'rmdir', 'rm', 'mv', 'cp', 'ln', 'sync', 'cat', 'stat', 'mktemp', 'id',
         'dirname', 'basename', 'find', 'rsync', 'install', 'readlink', 'awk', 'wc',
         'tr', 'cmp', 'grep', 'uname', 'sw_vers', 'lsof', 'echo']

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

with tempfile.TemporaryDirectory(prefix='hamn-system-install-') as tmp:
    work = Path(tmp).resolve()
    home, scratch = work / 'home', work / 'tmp'
    home.mkdir(); scratch.mkdir()
    # A source checkout with no completed build must not fall through to an
    # unrelated bin/hamn, even when that file is executable.
    incomplete = work / 'incomplete-source'
    (incomplete / 'bin').mkdir(parents=True)
    (incomplete / 'Cargo.toml').write_text('[package]\n')
    foreign = incomplete / 'bin/hamn'
    foreign.write_text('#!/bin/sh\ntouch "' + str(work / 'foreign-executed') + '"\n')
    foreign.chmod(0o755)
    rejected = subprocess.run(['/bin/bash', '-c', 'ROOT="$1"; source "$2"; install_support hash "$3"',
        'resolve', str(incomplete), str(ROOT / 'scripts/install-support.sh'), str(HAMN)], capture_output=True)
    assert rejected.returncode != 0 and not (work / 'foreign-executed').exists()
    release = work / 'release'
    (release / 'bin').mkdir(parents=True)
    shutil.copy2(HAMN, release / 'bin/hamn')
    for name in ('scripts', 'packaging'):
        shutil.copytree(ROOT / name, release / name)
    (release / 'packaging/release/update-manifest-url').write_text('https://example.invalid/manifest\n')
    (release / 'scripts/기록.txt').write_text('legacy receipt Unicode fixture\n')
    archive, guest = work / 'host.tar.gz', work / 'guest.img'
    with tarfile.open(archive, 'w:gz') as bundle:
        bundle.add(release, arcname='release')
    guest.write_bytes(b'isolated managed image fixture\n')
    version = subprocess.check_output([HAMN, '--version'], text=True).strip().split()[1]
    template = (ROOT / 'packaging/release/install.sh.in').read_text()
    fields = {'VERSION': 'v' + version, 'COMMIT': 'a' * 40, 'HOST_URL': archive.as_uri(),
              'HOST_SHA256': digest(archive), 'HOST_SIZE': str(archive.stat().st_size),
              'GUEST_URL': guest.as_uri(), 'GUEST_SHA256': digest(guest), 'GUEST_SIZE': str(guest.stat().st_size)}
    for key, value in fields.items():
        template = template.replace('__HAMN_' + key + '__', shlex.quote(value))
    installer = work / 'install.sh'; installer.write_text(template)

    # Enumerate only immutable OS executable locations; do not permit /usr/bin
    # as a whole (python3 and xcrun there can be developer-tool installation shims).
    allowed = {str(HAMN)}
    for tool in TOOLS:
        found = False
        for parent in ('/bin', '/usr/bin', '/sbin', '/usr/sbin'):
            candidate = Path(parent) / tool
            if candidate.exists():
                resolved = candidate.resolve()
                assert str(resolved).startswith(('/bin/', '/usr/bin/', '/sbin/', '/usr/sbin/'))
                assert resolved.stat().st_uid == 0 and not resolved.stat().st_mode & 0o022
                allowed.update((str(candidate), str(resolved)))
                found = True
        assert found, 'missing OS command: ' + tool
    profile = work / 'system-only.sb'
    profile.write_text('(version 1)\n(allow default)\n(deny process-exec)\n' +
        '(deny file-read* (subpath "/Library/Developer") (subpath "/Applications/Xcode.app"))\n' +
        '(allow process-exec\n' + ''.join(' (literal ' + json.dumps(p) + ')\n' for p in sorted(allowed)) +
        ' (regex ' + json.dumps('^' + re.escape(str(work)) + r'/.*/(bin/hamn|hamn-support)$') + ')\n' +
        ' (regex ' + json.dumps('^' + re.escape(str(work)) + r'/.*/scripts/(install-host|update-host)\.sh$') + '))\n')
    env = dict(os.environ, LC_ALL='C', HOME=str(home), TMPDIR=str(scratch), PATH='/usr/bin:/bin:/usr/sbin:/sbin',
               HAMN_INSTALL_ALLOW_LOCAL_ARTIFACTS='1', HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS='1')

    def run(*command, success=True):
        p = subprocess.run(['/usr/bin/sandbox-exec', '-f', str(profile), *map(str, command)],
                           env=env, capture_output=True, timeout=90)
        if success:
            assert p.returncode == 0, (command, p.returncode, p.stdout.decode(errors='replace'), p.stderr.decode(errors='replace'))
        return p

    # Prove the enforcement boundary before claiming a dependency-free install.
    for forbidden in ('/usr/bin/python3', '/usr/bin/perl', '/usr/bin/ruby', '/usr/bin/xcrun', '/usr/bin/git'):
        p = run(forbidden, '--version', success=False)
        assert p.returncode != 0, 'external/developer runtime was permitted: ' + forbidden
    run('/usr/bin/curl', '--version')
    for attempt in range(3):
        if attempt:
            active = (home / '.local/bin/hamn').resolve()
            (active.parent.parent / '.hamn-release.json').unlink()
        run('/bin/bash', installer)
        assert digest((home / '.local/bin/hamn').resolve()) == digest(HAMN)
    generations = list((home / '.local/share/hamn/src/.hamn-generations').glob('*/.hamn-generation'))
    assert len(generations) == 2, 'cleanup was skipped or lost retention during system-only install'
    active = (home / '.local/bin/hamn').resolve()
    selection = home / '.hamn/cache/guest-image.json'
    before = selection.read_bytes()
    # Independently reproduce the previous published Python receipt contract.
    # The native updater must accept it without reinstalling, including Unicode.
    entries = []
    def legacy_tree(path, name):
        info = path.lstat()
        entries.append((name, stat.S_IMODE(info.st_mode), None if path.is_dir() else digest(path)))
        if path.is_dir():
            for child in sorted(path.iterdir()):
                legacy_tree(child, name + '/' + child.name)
    generation = active.parent.parent
    legacy_tree(generation / 'bin', 'bin')
    for name in ('scripts', 'packaging'):
        legacy_tree(generation / 'share/hamn/src' / name, name)
    receipt_path = generation / '.hamn-release.json'
    receipt = json.loads(receipt_path.read_text())
    receipt['installedSHA256'] = hashlib.sha256(json.dumps(entries, separators=(',', ':')).encode()).hexdigest()
    receipt_path.write_text(json.dumps(receipt, sort_keys=True, separators=(',', ':')) + '\n')
    manifest = work / 'manifest.json'
    manifest.write_text(json.dumps({'schemaVersion': 2, 'channel': 'stable', 'version': 'v' + version,
        'commit': 'a' * 40, 'validationMode': 'github-hosted-no-vm',
        'compatibility': {'os': 'darwin', 'architecture': 'arm64', 'minimumMacOS': '13.0'},
        'artifacts': {'host': {'url': archive.as_uri(), 'sha256': digest(archive)},
                      'guestImage': {'url': guest.as_uri(), 'sha256': digest(guest)}}}))
    p = run(home / '.local/bin/hamn', '--headless', 'system', 'update', '--yes', '--manifest', manifest)
    assert p.stderr.endswith(b' is up to date.\n'), p.stderr
    assert (home / '.local/bin/hamn').resolve() == active and selection.read_bytes() == before
    bad = manifest.read_text().replace('"schemaVersion": 2', '"schemaVersion": 2, "schemaVersion": 2')
    manifest.write_text(bad)
    p = run(home / '.local/bin/hamn', '--headless', 'system', 'update', '--yes', '--manifest', manifest, success=False)
    assert p.returncode != 0
    assert (home / '.local/bin/hamn').resolve() == active and selection.read_bytes() == before
    assert not (home / '.hamn/profiles').exists()

print('OK: bootstrap, reinstall, native cleanup, unchanged update and rejection with only OS tools + hamn; Python/Perl/Ruby/CLT execution denied')
