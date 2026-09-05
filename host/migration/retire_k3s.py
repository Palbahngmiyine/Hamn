"""Fixed guest payload embedded in the signed host; no runtime path overrides."""
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile

JOURNAL = Path('/var/lib/hamn/k3s-retirement-v1.json')
TRANSACTIONS = Path('/var/lib/hamn/deployment-transactions')
UNIT = Path('/etc/systemd/system/k3s.service')
DATA = ('/var/lib/rancher/k3s', '/etc/rancher/k3s',
        '/var/lib/hamn/k3s-cni-transaction')
FILES = ('/usr/local/bin/k3s', '/usr/local/libexec/hamn/configure-k3s',
         '/usr/local/libexec/hamn/install-k3s', '/etc/hamn/k3s-compatibility.json',
         '/etc/hamn/k3s-compatibility.json.sig')
LINKS = {
    '/etc/cni/net.d/10-flannel.conflist': '/var/lib/rancher/k3s/agent/etc/cni/net.d/10-flannel.conflist',
    '/opt/cni/bin/flannel': '/var/lib/rancher/k3s/data/cni/flannel',
    '/opt/cni/bin/bandwidth': '/var/lib/rancher/k3s/data/cni/bandwidth',
}
HELPERS = ('verify-image-contract', 'guest-deployment-transaction', 'configure-docker')
STAGES = ('verified', 'stopped', 'resources', 'removed', 'helpers', 'complete')


def safe(path, leaf_link=False):
    """Never traverse symlink parents or delete through an untrusted directory."""
    path = Path(path)
    for parent in reversed(path.parents):
        if parent.exists() or parent.is_symlink():
            info = parent.lstat()
            if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
                raise RuntimeError(f'unsafe parent: {parent}')
    if path.exists() or path.is_symlink():
        info = path.lstat()
        if info.st_uid != 0 or (stat.S_ISLNK(info.st_mode) and not leaf_link):
            raise RuntimeError(f'unsafe owned path: {path}')
        if stat.S_ISREG(info.st_mode) and info.st_nlink != 1:
            raise RuntimeError(f'hard-linked owned file: {path}')
    return path


def atomic(path, data, mode=0o600):
    path = safe(path)
    fd, name = tempfile.mkstemp(prefix='.hamn-retire-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as output:
            output.write(data)
            output.flush()
            os.fchmod(output.fileno(), mode)
            os.fsync(output.fileno())
        os.replace(name, path)
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def run(*args, check=True):
    result = subprocess.run(args, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=60, check=False,
                            env={'PATH': '/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin',
                                 'LC_ALL': 'C'})
    if check and result.returncode:
        raise RuntimeError(f'{args[0]} {args[1:]} failed: {result.stderr[-2048:]!r}')
    return result


def ctr(*args, check=True):
    return run('ctr', '--address', '/run/containerd/containerd.sock',
               '--namespace', 'k8s.io', *args, check=check)


def identifiers(output, header=False):
    lines = output.decode('utf-8', errors='strict').splitlines()
    if header:
        lines = lines[1:]
    values = [line.split()[0] for line in lines if line.strip()]
    if len(values) > 20000 or any(not value or value.startswith('-') for value in values):
        raise RuntimeError('invalid or excessive containerd resource identifiers')
    return values


def resources():
    # ctr operates only on metadata scoped to k8s.io. Never remove content blobs,
    # containerd's database, shared snapshots on disk, or the moby namespace.
    for group, header, flags in [('tasks', True, ('--force',)),
                                 ('containers', False, ()), ('images', False, ())]:
        listing = ctr(group, 'list', *(() if header else ('--quiet',))).stdout
        for name in identifiers(listing, header):
            ctr(group, 'delete', *flags, '--', name)
    for name in identifiers(ctr('leases', 'list', '--quiet').stdout):
        ctr('leases', 'delete', '--', name)
    # Snapshot deletion must proceed from children to parents, with no busy loop.
    remaining = identifiers(ctr('snapshots', 'list').stdout, True)
    while remaining:
        failed = [name for name in remaining if
                  ctr('snapshots', 'remove', '--', name, check=False).returncode]
        if len(failed) == len(remaining):
            raise RuntimeError('k8s.io snapshots remain in use; migration is incomplete')
        remaining = failed
    for group, header in [('tasks', True), ('containers', False), ('images', False), ('leases', False)]:
        if identifiers(ctr(group, 'list', *(() if header else ('--quiet',))).stdout, header):
            raise RuntimeError(f'k8s.io {group} appeared during retirement')


def remove_data():
    mounts = [line.split()[4].replace('\\040', ' ').replace('\\134', '\\')
              for line in Path('/proc/self/mountinfo').read_text().splitlines()]
    for value in DATA:
        path = safe(value)
        if any(mount == value or mount.startswith(value + '/') for mount in mounts):
            raise RuntimeError(f'refusing to delete a mounted path: {path}')
        if path.exists():
            if not path.is_dir():
                raise RuntimeError(f'expected owned directory: {path}')
            shutil.rmtree(path)  # fd-based implementation does not follow child symlinks
    for value, expected in LINKS.items():
        path = safe(value, leaf_link=True)
        if path.is_symlink() and os.readlink(path) == expected:
            path.unlink()
        elif path.exists() or path.is_symlink():
            raise RuntimeError(f'refusing altered K3s CNI link: {path}')
    for value in FILES:
        path = safe(value)
        if path.exists():
            if not path.is_file():
                raise RuntimeError(f'expected owned file: {path}')
            path.unlink()


def recover_deployment():
    """Resolve an old rollback backup before it can restore retired K3s helpers."""
    root = safe(TRANSACTIONS)
    if not root.exists():
        return
    entries = list(root.iterdir())
    if not entries:
        return
    if JOURNAL.exists() or len(entries) != 1:
        raise RuntimeError('deployment backup conflicts with retirement; recovery is required')
    entry = safe(entries[0])
    if not entry.is_dir() or not re.fullmatch(r'[0-9a-f]{32}', entry.name):
        raise RuntimeError('invalid deployment backup')
    phase = safe(entry / 'phase')
    if not phase.is_file() or phase.stat().st_size > 32 or phase.read_text().strip() != 'ready':
        raise RuntimeError('incomplete deployment backup; recover it before K3s retirement')
    helper = safe('/usr/local/libexec/hamn/guest-deployment-transaction')
    run('bash', str(helper), 'rollback', entry.name)
    if entry.exists():
        raise RuntimeError('deployment recovery did not remove its backup')


def docker_ready():
    response = run('curl', '--fail', '--silent', '--max-time', '15',
                   '--unix-socket', '/var/run/docker.sock', 'http://localhost/_ping')
    if response.stdout.strip() != b'OK':
        raise RuntimeError('Docker did not report ready after retirement')


def migrate(payload):
    if os.geteuid() != 0:
        raise RuntimeError('guest retirement requires root')
    safe(JOURNAL)
    recover_deployment()
    JOURNAL.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    if JOURNAL.exists():
        state = json.loads(JOURNAL.read_bytes())
        if set(state) != {'version', 'stage'} or type(state['version']) is not int or state['version'] != 1 or state['stage'] not in STAGES:
            raise RuntimeError('invalid retirement journal')
        stage = STAGES.index(state['stage'])
    else:
        safe(UNIT)
        if UNIT.exists() and hashlib.sha256(UNIT.read_bytes()).hexdigest() != payload['unitSha256']:
            raise RuntimeError('K3s unit is not the Hamn-owned legacy unit')
        if not UNIT.exists() and any(Path(path).exists() for path in DATA + FILES):
            raise RuntimeError('K3s ownership cannot be established')
        for value in DATA + FILES:
            safe(value)
        binary = Path('/usr/local/bin/k3s')
        if binary.exists():
            manifest = json.loads(Path('/etc/hamn/k3s-compatibility.json').read_bytes())
            with binary.open('rb') as source:
                digest = hashlib.file_digest(source, 'sha256').hexdigest()
            if digest != manifest['binary']['sha256']:
                raise RuntimeError('refusing to delete an altered K3s binary')
        stage = -1
    actions = (
        lambda: None,
        stop,
        resources,
        remove_data,
        lambda: replace_helpers(payload),
        docker_ready,
    )
    for index in range(stage + 1, len(STAGES)):
        actions[index]()
        atomic(JOURNAL, json.dumps({'version': 1, 'stage': STAGES[index]}).encode())
    if stage == len(STAGES) - 1:
        docker_ready()  # A retry must not publish a ready profile while Docker is down.


def stop():
    if UNIT.exists() and not UNIT.is_symlink():
        run('systemctl', 'stop', 'k3s.service')
        run('systemctl', 'disable', 'k3s.service')
        safe(UNIT).unlink()
    # A regular unit cannot be masked until it has been removed.
    run('systemctl', 'mask', 'k3s.service')
    run('systemctl', 'daemon-reload')


def replace_helpers(payload):
    if set(payload['helpers']) != set(HELPERS):
        raise RuntimeError('invalid retirement helper payload')
    for name, content in payload['helpers'].items():
        atomic(Path('/usr/local/libexec/hamn') / name, content.encode(), 0o755)
    # Existing users are not necessarily updated by cloud-init on later boots.
    run('usermod', '--append', '--groups', 'docker', 'hamn')
