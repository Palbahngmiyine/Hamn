"""Fixed guest payload embedded in the signed host; no runtime path overrides."""
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile

JOURNAL = Path('/var/lib/hamn/k3s-retirement-v1.json')
PODS = Path('/var/lib/hamn/k3s-retirement-pods-v1.json')
TRANSACTIONS = Path('/var/lib/hamn/deployment-transactions')
UNIT = Path('/etc/systemd/system/k3s.service')
DATA = ('/var/lib/rancher/k3s', '/etc/rancher/k3s',
        '/var/lib/hamn/k3s-cni-transaction', '/var/lib/kubelet',
        '/var/lib/cni/networks/cbr0', '/run/flannel')
FILES = ('/usr/local/bin/k3s', '/usr/local/libexec/hamn/configure-k3s',
         '/usr/local/libexec/hamn/install-k3s', '/etc/hamn/k3s-compatibility.json',
         '/etc/hamn/k3s-compatibility.json.sig')
LINKS = {
    '/etc/cni/net.d/10-flannel.conflist': '/var/lib/rancher/k3s/agent/etc/cni/net.d/10-flannel.conflist',
    '/opt/cni/bin/flannel': '/var/lib/rancher/k3s/data/cni/flannel',
    '/opt/cni/bin/bandwidth': '/var/lib/rancher/k3s/data/cni/bandwidth',
}
HELPERS = ('verify-image-contract', 'guest-deployment-transaction', 'configure-docker')
# Reviewed helper identities shipped before transaction provenance was recorded.
LEGACY_HELPERS = {
    'verify-image-contract': '4c9f43fceca8c52710cef6252d0db9216b3dde1f765454ab8ee979d17995dc67',
    'guest-deployment-transaction': '912bfb92669467768ae7eecae07ef2f405d4f6d0308eb81431595b101dd9c901',
    'configure-docker': '22f4a83728959ebba1e6a56a41efb9910c656067e7a6ff544bda09c9d8ce43ca',
}
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


def run(*args, check=True, timeout=60):
    result = subprocess.run(args, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=timeout, check=False,
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
    retire_pods()
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
        # Image removal also schedules containerd GC. A snapshot that vanished
        # between listing and deletion is already retired, not a busy resource.
        current = identifiers(ctr('snapshots', 'list').stdout, True)
        failed = [name for name in failed if name in current]
        if len(failed) == len(remaining):
            raise RuntimeError('k8s.io snapshots remain in use; migration is incomplete')
        remaining = failed
    for group, header in [('tasks', True), ('containers', False), ('images', False), ('leases', False)]:
        if identifiers(ctr(group, 'list', *(() if header else ('--quiet',))).stdout, header):
            raise RuntimeError(f'k8s.io {group} appeared during retirement')


def mountpoints():
    return [line.split()[4].replace('\\040', ' ').replace('\\134', '\\')
            for line in Path('/proc/self/mountinfo').read_text().splitlines()]


def retire_pods():
    """CRI owns sandbox networking; ctr alone leaves its metadata and netns."""
    if not safe('/usr/local/bin/k3s').exists():
        return
    def cri(*args):
        return run('/usr/local/bin/k3s', 'crictl', '--runtime-endpoint',
                   'unix:///run/containerd/containerd.sock', *args)
    def records(command, key):
        rows = json.loads(cri(command, '-o', 'json').stdout)[key]
        if not isinstance(rows, list) or len(rows) > 20000:
            raise RuntimeError('invalid CRI resource list')
        for row in rows:
            if not isinstance(row, dict) or not re.fullmatch(r'[0-9a-f]{64}', row.get('id', '')):
                raise RuntimeError('invalid CRI resource identity')
        return rows
    pods = records('pods', 'items')
    uids = [pod.get('metadata', {}).get('uid', '') for pod in pods]
    if safe(PODS).exists():
        saved = json.loads(PODS.read_bytes())
        if not isinstance(saved, list):
            raise RuntimeError('invalid retirement Pod inventory')
        uids += saved
    if len(uids) > 40000 or any(not isinstance(uid, str) or
            not re.fullmatch(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', uid) for uid in uids):
        raise RuntimeError('invalid retirement Pod UID')
    uids = sorted(set(uids))
    atomic(PODS, json.dumps(uids).encode())
    for pod in pods:
        cri('stopp', pod['id'])
        cri('rmp', '--force', pod['id'])
    if records('pods', 'items'):
        raise RuntimeError('CRI sandboxes remain after retirement')
    # Kubelet is stopped. Only detach volume mounts belonging to captured Pod
    # UIDs; never delete the backing hostPath or a shared Docker mount.
    for uid in uids:
        directory = safe('/var/lib/kubelet/pods/' + uid)
        mounts = mountpoints()
        if str(directory) in mounts:
            raise RuntimeError('refusing a mounted Pod directory')
        for mount in sorted((path for path in mounts if path.startswith(str(directory) + '/')),
                            key=lambda path: path.count('/'), reverse=True):
            safe(mount)
            run('umount', '--', mount)
        if directory.exists():
            shutil.rmtree(directory)


def remove_data():
    mounts = mountpoints()
    # Validate every path before removing any directory. Unexpected mounts
    # outside captured Pod UIDs must preserve the entire remaining state.
    for value in DATA:
        path = safe(value)
        if any(mount == value or mount.startswith(value + '/') for mount in mounts):
            raise RuntimeError(f'refusing to delete a mounted path: {path}')
    for value in DATA:
        path = safe(value)
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


def backup_path(entry, path):
    # configure-containerd installs precisely these distribution-owned CNI links.
    # cp -a preserves the link itself; never follow arbitrary backup links.
    relative = path.relative_to(entry)
    plugins = {'bridge', 'firewall', 'host-local', 'loopback', 'portmap', 'tuning'}
    if path.is_symlink() and relative.parent == Path('data/cni_bin') and path.name in plugins:
        safe(path, leaf_link=True)
        target = Path('/usr/lib/cni') / path.name
        if os.readlink(path) != str(target):
            raise RuntimeError(f'altered CNI backup link: {path}')
        target = safe(target)
        info = target.stat()
        if not stat.S_ISREG(info.st_mode) or info.st_mode & 0o022 or not info.st_mode & 0o111:
            raise RuntimeError(f'unsafe CNI executable: {target}')
        return path
    if path.is_symlink():
        raise RuntimeError(f'unsafe deployment backup link: {path}')
    return safe(path)


def recovery_identity(entry, payload):
    """Only restore a complete, owned backup of the same deployment contract."""
    journal = json.loads(safe(JOURNAL).read_bytes()) if JOURNAL.exists() else None
    expected = {name: hashlib.sha256(content.encode()).hexdigest()
                for name, content in payload['helpers'].items()}
    actual = {}
    # Validate the entire restore tree, including nested paths cp -a will visit.
    for directory, directories, files in os.walk(entry, followlinks=False):
        for name in directories + files:
            path = backup_path(entry, Path(directory) / name)
            info = path.lstat()
            if not (stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode)):
                raise RuntimeError('unsafe deployment backup entry')
    # Validate every item before rollback can replace any live file.
    directories = {'libexec_hamn', 'etc_hamn', 'docker_dropin', 'cni_bin'}
    items = directories | {'hamnd', 'hamnd_unit', 'containerd_config', 'docker_config',
                           'host_dns_config', 'host_dns_unit', 'modules_config', 'sysctl_config'}
    for name in items:
        metadata = safe(entry / 'meta' / name)
        if not metadata.is_file() or metadata.stat().st_size > 16:
            raise RuntimeError(f'missing deployment backup metadata: {name}')
        state = metadata.read_text().strip()
        data = entry / 'data' / name
        if state not in ('present', 'absent') or (state == 'absent' and data.exists()):
            raise RuntimeError(f'invalid deployment backup metadata: {name}')
        if state == 'present' and not (data.is_dir() if name in directories else data.is_file()):
            raise RuntimeError(f'incomplete deployment backup data: {name}')
    for service in ('hamnd', 'containerd', 'docker', 'hamn-host-dns'):
        for suffix, allowed in [('active', {'active', 'inactive'}),
                                ('enabled', {'enabled', 'enabled-runtime', 'disabled', 'masked',
                                             'masked-runtime', 'static', 'indirect', 'generated',
                                             'transient', 'not-found'})]:
            metadata = safe(entry / 'meta' / f'{service}.service.{suffix}')
            if not metadata.is_file() or metadata.stat().st_size > 32 or metadata.read_text().strip() not in allowed:
                raise RuntimeError(f'invalid deployment service metadata: {service}.{suffix}')
    for name in HELPERS:
        helper = safe(entry / 'data/libexec_hamn' / name)
        if not helper.is_file():
            raise RuntimeError('deployment backup helper is missing')
        actual[name] = hashlib.sha256(helper.read_bytes()).hexdigest()
    provenance = safe(entry / 'provenance.json')
    if provenance.exists():
        saved = json.loads(provenance.read_bytes())
        if saved != {'version': 1, 'retirement': journal, 'helpers': actual} or actual != expected:
            raise RuntimeError('deployment backup belongs to a different contract or retirement stage')
    elif actual != expected and actual != LEGACY_HELPERS:
        raise RuntimeError('legacy deployment backup helper identity is not trusted')
    if journal is not None:
        if journal != {'version': 1, 'stage': 'complete'}:
            raise RuntimeError('deployment backup conflicts with unfinished retirement')
        forbidden = ('data/libexec_hamn/configure-k3s', 'data/libexec_hamn/install-k3s',
                     'data/etc_hamn/k3s-compatibility.json',
                     'data/etc_hamn/k3s-compatibility.json.sig',
                     'data/cni_bin/flannel', 'data/cni_bin/bandwidth')
        if any((entry / name).exists() or (entry / name).is_symlink() for name in forbidden):
            raise RuntimeError('deployment backup could restore retired K3s files')


def recover_deployment(payload=None):
    """Resolve an old rollback backup before it can restore retired K3s helpers."""
    root = safe(TRANSACTIONS)
    if not root.exists():
        return
    entries = list(root.iterdir())
    if not entries:
        return
    if len(entries) != 1:
        raise RuntimeError('deployment backup conflicts with retirement; recovery is required')
    entry = safe(entries[0])
    if not entry.is_dir() or not re.fullmatch(r'[0-9a-f]{32}', entry.name):
        raise RuntimeError('invalid deployment backup')
    phase = safe(entry / 'phase')
    if not phase.is_file() or phase.stat().st_size > 32 or phase.read_text().strip() != 'ready':
        raise RuntimeError('incomplete deployment backup; recover it before K3s retirement')
    if payload is not None:
        recovery_identity(entry, payload)
    elif JOURNAL.exists():
        raise RuntimeError('deployment recovery requires the signed helper contract')
    helper = safe('/usr/local/libexec/hamn/guest-deployment-transaction')
    if payload is not None:
        digest = hashlib.sha256(helper.read_bytes()).hexdigest()
        expected = hashlib.sha256(payload['helpers']['guest-deployment-transaction'].encode()).hexdigest()
        if digest not in (expected, LEGACY_HELPERS['guest-deployment-transaction']):
            raise RuntimeError('installed recovery helper does not match the signed contract')
    run('flock', '--wait', '120', '/run/hamn-deployment.lock',
        'timeout', '--kill-after=5s', '60s', 'bash', str(helper), 'rollback', entry.name, timeout=190)
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
    recover_deployment(payload)
    if sys.argv[1:] == ['recover-only']:
        replace_helpers(payload)
        return
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
        replace_helpers(payload)
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
