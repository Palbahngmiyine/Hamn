"""Opt-in real Engine transport checks; never start a VM or use user credentials.

TLS terminates at an owned loopback mTLS proxy, which forwards bytes to the
existing profile Unix socket. SSH reaches the owned VM's real Docker CLI with a
copied profile key and a private pinned-host-key configuration. Neither path
changes the daemon configuration. Credentials exist only in the temporary tree.
"""
import ctypes
import errno
import hashlib
import json
import os
from pathlib import Path
import select
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time

from workspace_live_transport import BsdInfo


def birth(pid):
    """Observe identity without signalling a possibly reused PID."""
    library = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
    info = BsdInfo()
    if library.proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info)) != ctypes.sizeof(info):
        return None
    assert info.pid == pid and info.uid == os.getuid()
    return {'pid': pid, 'startSec': info.startSec, 'startUsec': info.startUsec}


def gone(records, timeout=5):
    deadline = time.monotonic() + timeout
    while True:
        remaining = [record for record in records if birth(record['pid']) == record]
        if not remaining:
            return
        assert time.monotonic() < deadline, ('owned transport processes survived', remaining)
        threading.Event().wait(0.02)


class Proxy:
    """A bounded raw transport, with explicit handshake/stall/cleanup witnesses."""
    def __init__(self, target=None, context=None):
        self.target, self.context = target, context
        self.listener = socket.socket()
        self.listener.bind(('127.0.0.1', 0))
        self.listener.listen()
        self.listener.settimeout(0.1)
        self.port = self.listener.getsockname()[1]
        self.closed, self.entered, self.release = threading.Event(), threading.Event(), threading.Event()
        self.stall = False
        self.lock = threading.Lock()
        self.sockets, self.workers = set(), []
        self.accepted = self.forwarded = self.tls_failures = 0
        self.errors = []
        self.thread = threading.Thread(target=self.accept, name='owned-engine-proxy')
        self.thread.start()

    def accept(self):
        while not self.closed.is_set():
            try:
                connection, _ = self.listener.accept()
            except socket.timeout:
                continue
            except OSError as error:
                if not self.closed.is_set():
                    self.errors.append(repr(error))
                return
            with self.lock:
                self.accepted += 1
                self.sockets.add(connection)
            worker = threading.Thread(target=self.handle, args=(connection,))
            self.workers.append(worker)
            worker.start()

    def handle(self, connection):
        backend = None
        try:
            connection.settimeout(5)
            if self.context:
                raw = connection
                try:
                    connection = self.context.wrap_socket(raw, server_side=True)
                except ssl.SSLError:
                    with self.lock:
                        self.tls_failures += 1
                    return
                finally:
                    with self.lock:
                        self.sockets.discard(raw)
                        self.sockets.add(connection)
            if self.target is None:
                connection.sendall(b'HTTP/1.1 503 Decoy\r\nContent-Length: 0\r\nConnection: close\r\n\r\n')
                return
            if self.stall:
                assert connection.recv(65536), 'stalled transport received no request'
                self.entered.set()
                assert self.release.wait(15), 'stalled fixture was not released'
                try:
                    connection.shutdown(socket.SHUT_WR)
                except OSError as error:
                    if error.errno not in (errno.ENOTCONN, errno.ECONNRESET, errno.EPIPE):
                        raise
                return
            backend = socket.socket(socket.AF_UNIX)
            backend.settimeout(5)
            backend.connect(str(self.target))
            with self.lock:
                self.sockets.add(backend)
                self.forwarded += 1
            peers = {connection: backend, backend: connection}
            deadline = time.monotonic() + 15
            while not self.closed.is_set():
                assert time.monotonic() < deadline, 'proxy transfer exceeded fixture deadline'
                ready = [connection] if isinstance(connection, ssl.SSLSocket) and connection.pending() else select.select(list(peers), [], [], 0.1)[0]
                for source in ready:
                    data = source.recv(65536)
                    if not data:
                        return
                    peers[source].sendall(data)
        except (BrokenPipeError, ConnectionResetError, ssl.SSLEOFError):
            pass  # Docker closes its transport after consuming the API response.
        except Exception as error:
            if not self.closed.is_set():
                self.errors.append(repr(error))
        finally:
            for channel in (connection, backend):
                if channel:
                    with self.lock:
                        self.sockets.discard(channel)
                    channel.close()

    def close(self):
        self.closed.set()
        self.release.set()
        self.listener.close()
        self.thread.join(3)
        with self.lock:
            channels = list(self.sockets)
        for channel in channels:
            try:
                channel.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        for worker in self.workers:
            worker.join(6)
        assert not self.thread.is_alive() and not any(worker.is_alive() for worker in self.workers)
        assert not self.sockets and not self.errors, (self.sockets, self.errors)


def certificates(directory):
    def openssl(*args):
        subprocess.run(['/usr/bin/openssl', *map(str, args)], check=True,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20)
    ca_config = directory / 'ca.conf'
    ca_config.write_text('[req]\ndistinguished_name=dn\nx509_extensions=ca\n[dn]\n[ca]\n'
        'basicConstraints=critical,CA:true\nkeyUsage=critical,keyCertSign,cRLSign\n'
        'subjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid:always\n')
    for authority in ('ca', 'wrong-ca'):
        openssl('req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
                '-config', ca_config,
                '-subj', '/CN=hamn-owned-' + authority, '-keyout', directory / (authority + '.key'),
                '-out', directory / (authority + '.pem'))
    for name, usage in [('server', 'serverAuth'), ('client', 'clientAuth')]:
        openssl('req', '-new', '-newkey', 'rsa:2048', '-nodes', '-subj', '/CN=hamn-owned-' + name,
                '-keyout', directory / (name + '.key'), '-out', directory / (name + '.csr'))
        extension = directory / (name + '.ext')
        extension.write_text('basicConstraints=critical,CA:false\n'
            'keyUsage=critical,digitalSignature,keyEncipherment\n'
            'subjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid,issuer\n'
            'extendedKeyUsage=' + usage + '\nsubjectAltName=IP:127.0.0.1\n')
        openssl('x509', '-req', '-in', directory / (name + '.csr'), '-CA', directory / 'ca.pem',
                '-CAkey', directory / 'ca.key', '-CAcreateserial', '-days', '1',
                '-extfile', extension, '-out', directory / (name + '.pem'))
    for key in directory.glob('*.key'):
        key.chmod(0o600)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.verify_mode = ssl.CERT_REQUIRED
    context.load_cert_chain(directory / 'server.pem', directory / 'server.key')
    context.load_verify_locations(directory / 'ca.pem')
    return context


def wrapper(path, executable, arguments, witness):
    # Executing the actual programs preserves their transport. The witness
    # records only birth identities, never key contents, environment or argv.
    path.write_text('#!' + sys.executable + '\n' +
        'import json, os, sys\n' +
        'sys.path.insert(0, ' + repr(str(Path(__file__).parent)) + ')\n' +
        'from workspace_live_external_contexts import birth\n' +
        'with open(' + repr(str(witness)) + ', "a") as log:\n' +
        '    log.write(json.dumps(birth(os.getpid())) + "\\n")\n' +
        'os.execv(' + repr(str(executable)) + ', ' + repr([str(executable), *map(str, arguments)]) + ' + sys.argv[1:])\n')
    path.chmod(0o700)


def external_contexts(root, runtime):
    """Caller owns a running frozen verify profile with stable test containers."""
    root = Path(root)
    owner = json.loads((root / 'ownership.json').read_text())
    assert owner['profile'] == 'verify' and Path(owner['workspace']).resolve() == Path(__file__).resolve().parents[2]
    assert Path(owner['home']).resolve() == runtime.home.resolve() == (root / 'home').resolve()
    assert runtime.binary.resolve() == (root / 'hamn-under-test').resolve()
    profile = runtime.home / '.hamn/verify'
    vm_pid = (profile / 'vmrun.pid').read_bytes()
    socket_path = profile / 'docker.sock'
    assert socket_path.is_socket()
    evidence = {'binarySha256': hashlib.sha256(runtime.binary.read_bytes()).hexdigest(),
                'tlsBoundary': 'owned loopback mTLS proxy -> existing profile Unix socket -> real guest Docker Engine',
                'sshBoundary': 'real /usr/bin/ssh, copied owned profile key, private pinned host key -> real guest docker system dial-stdio',
                'sshPinOrigin': 'server public key read through the existing owned profile SSH bootstrap; not an independent out-of-band authenticity claim',
                'cases': []}
    output = root / 'external-contexts-results.json'
    with tempfile.TemporaryDirectory(prefix='external-contexts-', dir=root) as temporary:
        work = Path(temporary)
        home, config, tools = work / 'home', work / 'config', work / 'tools'
        for directory in (home, config, tools):
            directory.mkdir(mode=0o700)
        docker_log, ssh_log = work / 'docker-processes.jsonl', work / 'ssh-processes.jsonl'
        wrapper(tools / 'docker', runtime.docker, [], docker_log)
        env = {**runtime.environment, 'HOME': str(home), 'PATH': str(tools) + ':' + runtime.environment['PATH'],
               'DOCKER_CONFIG': str(work / 'wrong-config')}
        env.pop('DOCKER_API_VERSION', None)
        tls, decoy = Proxy(socket_path, certificates(work)), Proxy()
        env.update(DOCKER_HOST=f'tcp://127.0.0.1:{decoy.port}', DOCKER_CONTEXT='decoy')

        def direct(*args, success=True):
            result = subprocess.run([runtime.docker, '--config', config, *args], env=env,
                                    capture_output=True, text=True, timeout=15)
            assert (result.returncode == 0) == success, (args, result.stdout, result.stderr)
            return result.stdout

        def context(name, endpoint):
            direct('context', 'create', name, '--docker', endpoint)

        def invoke(name, success=True, error=None, timeout=10):
            command = [runtime.binary, '--headless', 'docker', 'containers', 'list', '--timeout', str(timeout),
                       '--docker-config', config]
            if name is not None:
                command += ['--context', name]
            result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=timeout + 5)
            value = json.loads(result.stdout)
            assert (result.returncode == 0) == success and value['ok'] == success, (name, result, value)
            if success:
                assert value['target']['context'] == name and value['target']['dockerConfig'] == str(config)
                assert value['target']['profile'] is None
                rows = sorted((item['Id'], item['State']) for item in value['data'])
                assert rows and all(len(identifier) == 64 and all(c in '0123456789abcdef' for c in identifier)
                                    for identifier, _ in rows)
                expected = sorted(tuple(json.loads(line)[key] for key in ('ID', 'State')) for line in
                    direct('--context', name, 'ps', '-a', '--no-trunc', '--format', '{{json .}}').splitlines())
                socket_rows = sorted(tuple(json.loads(line)[key] for key in ('ID', 'State')) for line in
                    runtime.engine('ps', '-a', '--no-trunc', '--format', '{{json .}}', profile='verify').splitlines())
                assert rows == expected == socket_rows, (name, rows, expected, socket_rows)
            elif error:
                assert value['error']['code'] == error, value
            evidence['cases'].append({'context': name, 'ok': success,
                'idsAndStates': rows if success else None, 'error': None if success else value['error']})
            output.write_text(json.dumps(evidence, indent=2))
            return value

        try:
            context('decoy', f'host=tcp://127.0.0.1:{decoy.port}')
            direct('context', 'use', 'decoy')
            endpoint = f'host=tcp://127.0.0.1:{tls.port}'
            context('tls', endpoint + f',ca={work}/ca.pem,cert={work}/client.pem,key={work}/client.key')
            context('tls-wrong-ca', endpoint + f',ca={work}/wrong-ca.pem,cert={work}/client.pem,key={work}/client.key')
            context('tls-no-client', endpoint + f',ca={work}/ca.pem')
            invoke('tls')
            forwarded = tls.forwarded
            for name in ('tls-wrong-ca', 'tls-no-client'):
                direct('--context', name, 'ps', success=False)
                invoke(name, success=False)
            assert tls.forwarded == forwarded and tls.tls_failures >= 4
            invoke('missing-context', success=False)
            invoke(None, success=False, error='invalidRequest')
            assert decoy.accepted == 0, 'explicit target fell back to poisoned/default endpoint'

            # A TLS-authenticated connection deliberately withholds the Engine
            # response. Hamn must return its own timeout and reap its CLI group.
            tls.stall = True
            invoke('tls', success=False, error='timeout', timeout=1)
            assert tls.entered.is_set()
            gone([json.loads(line) for line in docker_log.read_text().splitlines()])
            tls.stall = False
            tls.release.set()

            status = runtime.call('vm', 'status', profile='verify')
            ip = status['ip']
            server_key = runtime.ssh('cat /etc/ssh/ssh_host_ed25519_key.pub', profile='verify').strip().split()
            assert len(server_key) >= 2 and server_key[0] == 'ssh-ed25519'
            shutil.copyfile(profile / 'id_ed25519', work / 'ssh-key')
            (work / 'ssh-key').chmod(0o600)
            subprocess.run(['/usr/bin/ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', work / 'wrong-key'],
                           check=True, capture_output=True, timeout=10)
            wrong_key = (work / 'wrong-key.pub').read_text().split()
            (work / 'known-hosts').write_text('hamn-owned-engine ' + ' '.join(server_key[:2]) + '\n')
            (work / 'wrong-hosts').write_text('hamn-owned-engine ' + ' '.join(wrong_key[:2]) + '\n')
            ssh_config = work / 'ssh-config'
            ssh_config.write_text(''.join(
                f'Host {name}\n  HostName {ip}\n  User hamn\n  HostKeyAlias hamn-owned-engine\n'
                f'  IdentityFile "{key}"\n  UserKnownHostsFile "{hosts}"\n'
                '  GlobalKnownHostsFile /dev/null\n  StrictHostKeyChecking yes\n  CheckHostIP no\n'
                '  HostKeyAlgorithms ssh-ed25519\n  IdentitiesOnly yes\n  IdentityAgent none\n'
                '  BatchMode yes\n  ConnectTimeout 5\n  ConnectionAttempts 1\n'
                '  ControlMaster no\n  ControlPath none\n  LogLevel ERROR\n'
                for name, key, hosts in [('owned-engine', work / 'ssh-key', work / 'known-hosts'),
                    ('wrong-host', work / 'ssh-key', work / 'wrong-hosts'),
                    ('wrong-client', work / 'wrong-key', work / 'known-hosts')]))
            ssh_config.chmod(0o600)
            wrapper(tools / 'ssh', '/usr/bin/ssh', ['-F', ssh_config], ssh_log)
            for name in ('owned-engine', 'wrong-host', 'wrong-client'):
                context(name, 'host=ssh://' + name)
            invoke('owned-engine')
            for name in ('wrong-host', 'wrong-client'):
                direct('--context', name, 'ps', success=False)
                invoke(name, success=False)
            assert ssh_log.exists(), 'Docker did not execute the real SSH wrapper'
            gone([json.loads(line) for path in (docker_log, ssh_log) for line in path.read_text().splitlines()])
            assert decoy.accepted == 0
            assert not (home / '.hamn').exists(), 'external Docker operation touched managed VM state'
            assert (profile / 'vmrun.pid').read_bytes() == vm_pid
            evidence.update(tlsHandshakeFailures=tls.tls_failures, decoyConnections=decoy.accepted,
                            ownedTransportProcessesReaped=True, clientHamnDirectoryCreated=False,
                            managedVmPidUnchanged=True)
        finally:
            try:
                tls.close()
            finally:
                try:
                    decoy.close()
                finally:
                    output.write_text(json.dumps(evidence, indent=2))
    print('PASS: real Engine via explicit mTLS/SSH contexts, failed authentication, no fallback and owned cleanup', flush=True)
