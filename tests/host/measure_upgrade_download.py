#!/usr/bin/env python3
"""Opt-in native acquisition measurements with real files and loopback HTTPS.

HTTP payload bytes exclude TLS/HTTP framing. Logical bytes and st_blocks*512 are
file accounting, not APFS physical sharing or exclusive disk consumption. This
runs only private native acquisition, never installation, a VM, or public URLs.
"""
import argparse
import contextlib
import hashlib
import http.server
import json
import os
from pathlib import Path
import platform
import signal
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time
from datetime import datetime, timezone

ROOT = Path(__file__).resolve().parents[2]
CHUNK = 64 * 1024


def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(CHUNK), b''): result.update(chunk)
    return result.hexdigest()


def storage(path):
    entries = []
    for file in sorted(path.rglob('*')):
        info = file.lstat()
        assert not file.is_symlink(), 'unexpected cache symlink'
        if file.is_file():
            entries.append({'file': str(file.relative_to(path)), 'logicalBytes': info.st_size,
                            'allocatedBlockBytes': info.st_blocks * 512})
    return {'logicalBytes': sum(item['logicalBytes'] for item in entries),
            'allocatedBlockBytes': sum(item['allocatedBlockBytes'] for item in entries), 'files': entries}


class TransferServer:
    def __init__(self, artifact, sha256, certificate, key):
        self.artifact, self.sha256, self.size = artifact, sha256, artifact.stat().st_size
        self.requests, self.lock = [], threading.Lock()
        self.case, self.interrupt_at = '', None
        self.ready, self.release = threading.Event(), threading.Event()
        self.release.set()
        owner = self
        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'
            def log_message(self, *_args): pass
            def do_GET(self):
                self.connection.settimeout(30)
                record = {'case': owner.case, 'range': self.headers.get('Range'),
                          'ifRange': self.headers.get('If-Range'), 'payloadBytes': 0}
                try:
                    assert self.path == '/artifact', self.path
                    start, status = 0, 200
                    if record['range']:
                        assert record['range'].startswith('bytes=') and record['range'].endswith('-')
                        start = int(record['range'][6:-1]); status = 206
                        assert 0 < start < owner.size
                        assert record['ifRange'] == '"' + owner.sha256 + '"'
                    record['status'] = status
                    owner.ready.set()
                    assert owner.release.wait(30), 'concurrent client barrier timed out'
                    self.send_response(status)
                    self.send_header('Content-Length', str(owner.size - start))
                    self.send_header('ETag', '"' + owner.sha256 + '"')
                    self.send_header('Connection', 'close')
                    if status == 206:
                        self.send_header('Content-Range', f'bytes {start}-{owner.size - 1}/{owner.size}')
                    self.end_headers()
                    remaining = owner.size - start
                    if owner.interrupt_at is not None: remaining = min(remaining, owner.interrupt_at)
                    with owner.artifact.open('rb') as source:
                        source.seek(start)
                        while remaining:
                            chunk = source.read(min(CHUNK, remaining))
                            assert chunk, 'artifact truncated during measurement'
                            self.wfile.write(chunk)
                            record['payloadBytes'] += len(chunk)
                            remaining -= len(chunk)
                    self.wfile.flush()
                except Exception as error:
                    record['error'] = str(error)
                finally:
                    self.close_connection = True
                    with owner.lock: owner.requests.append(record)
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        context.load_cert_chain(certificate, key)
        self.server.socket = context.wrap_socket(self.server.socket, server_side=True)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.url = f'https://127.0.0.1:{self.server.server_port}/artifact'

    def begin(self, case, *, interrupt_at=None, gated=False):
        self.case, self.interrupt_at = case, interrupt_at
        self.ready.clear()
        self.release.clear() if gated else self.release.set()

    def observations(self, case):
        with self.lock: result = [dict(item) for item in self.requests if item['case'] == case]
        assert not any('error' in item for item in result), result
        return {'requests': result, 'requestCount': len(result),
                'httpPayloadBytes': sum(item['payloadBytes'] for item in result)}

    def close(self):
        self.release.set()
        self.server.shutdown(); self.server.server_close(); self.thread.join(timeout=10)
        assert not self.thread.is_alive(), 'owned HTTPS server did not stop'


def certificate(root):
    config = root / 'tls.conf'
    config.write_text('[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=ext\n'
                      '[dn]\nCN=localhost\n[ext]\nsubjectAltName=IP:127.0.0.1\n'
                      'basicConstraints=critical,CA:TRUE\nkeyUsage=critical,digitalSignature,keyEncipherment,keyCertSign\n'
                      'extendedKeyUsage=serverAuth\n')
    cert, key = root / 'certificate.pem', root / 'key.pem'
    subprocess.run(['/usr/bin/openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes',
                    '-days', '1', '-config', config, '-keyout', key, '-out', cert],
                   check=True, capture_output=True, timeout=30)
    key.chmod(0o600)
    return cert, key


def measure(binary, image, label, root, cert, key, partial_bytes):
    size, sha256 = image.stat().st_size, digest(image)
    assert 1 < size < 2 * 1024**3, 'measurement requires a guest artifact below 2 GiB'
    offset = min(partial_bytes, size // 4)
    assert offset > 0
    server = TransferServer(image, sha256, cert, key)
    processes = []
    env = {**os.environ, 'HOME': str(root), 'CURL_CA_BUNDLE': str(cert), 'SSL_CERT_FILE': str(cert),
           'NO_PROXY': '127.0.0.1', 'no_proxy': '127.0.0.1'}
    manifest = root / ('manifest-' + label + '.json')
    manifest.write_text(json.dumps({'schemaVersion': 3, 'channel': 'stable', 'version': 'v1.0.0',
        'commit': 'a' * 40, 'validationMode': 'github-hosted-no-vm',
        'compatibility': {'os': 'darwin', 'architecture': 'arm64', 'minimumMacOS': '13.0'},
        'artifacts': {'host': {'url': server.url, 'sha256': '0' * 64, 'size': 1},
            'guestImage': {'url': server.url, 'sha256': sha256, 'size': size, 'format': 'qcow2',
                           'compression': 'zlib', 'virtualSize': 8 * 1024**3}}}))
    manifest.chmod(0o600)
    report = {'label': label, 'imageSHA256': sha256, 'compressedBytes': size, 'scenarios': {}}

    def spawn(cache, counts):
        child = subprocess.Popen([binary, '__install-support', 'upgrade', 'acquire', manifest,
                                  'guestImage', cache, counts], env=env,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                  start_new_session=True)
        processes.append(child)
        return child

    def complete(child, counts, *, success=True):
        output, error = child.communicate(timeout=180)
        assert (child.returncode == 0) == success, (child.returncode, output, error)
        if not success:
            assert not counts.exists(), 'failed transfer reported completed counts'
            return {'exitCode': child.returncode, 'error': error.strip()}
        result = json.loads(counts.read_text())
        artifact = Path(output.strip())
        assert artifact.stat().st_size == size and digest(artifact) == sha256
        return result

    @contextlib.contextmanager
    def case_root(name):
        with tempfile.TemporaryDirectory(prefix=label + '-' + name + '-', dir=root) as path:
            directory = Path(path)
            cache = directory / 'cache'; cache.mkdir(mode=0o755)
            yield directory, cache

    def record(name, cache, counts, started, **extra):
        observed = server.observations(name)
        value = {**observed, 'nativeCounts': counts, 'cache': storage(cache),
                 'elapsedSeconds': round(time.monotonic() - started, 6), **extra}
        report['scenarios'][name] = value
        print(f'{label}/{name}: {observed["requestCount"]} requests, {observed["httpPayloadBytes"]} payload bytes',
              file=sys.stderr, flush=True)
        return value

    try:
        with case_root('cache') as (directory, cache):
            for name in ('cold', 'warm'):
                server.begin(name); started = time.monotonic()
                path = directory / (name + '-counts.json')
                counts = complete(spawn(cache, path), path)
                result = record(name, cache, counts, started)
                expected = size if name == 'cold' else 0
                assert result['httpPayloadBytes'] == counts['downloadedBytes'] == expected
                assert result['requestCount'] == (1 if name == 'cold' else 0)
                assert counts['source'] == ('download' if name == 'cold' else 'cache')
                assert counts['reusedBytes'] == (0 if name == 'cold' else size)
        with case_root('resume') as (directory, cache):
            server.begin('interrupted', interrupt_at=offset); started = time.monotonic()
            path = directory / 'interrupted-counts.json'
            failure = complete(spawn(cache, path), path, success=False)
            partial = cache / 'downloads' / ('.' + sha256 + '.partial')
            assert partial.stat().st_size == offset
            interrupted = record('interrupted', cache, None, started, failure=failure, offsetBytes=offset)
            assert interrupted['requestCount'] == 1 and interrupted['httpPayloadBytes'] == offset
            server.begin('resumed'); started = time.monotonic()
            path = directory / 'resumed-counts.json'
            counts = complete(spawn(cache, path), path)
            result = record('resumed', cache, counts, started, offsetBytes=offset)
            assert result['requestCount'] == 1 and result['httpPayloadBytes'] == size - offset
            assert counts['downloadedBytes'] == counts['resumedBytes'] == size - offset
            assert counts['reusedBytes'] == offset and counts['source'] == 'resumed'
            assert result['requests'][0]['range'] == f'bytes={offset}-'
            assert not partial.exists()
        with case_root('concurrent') as (directory, cache):
            server.begin('concurrent', gated=True); started = time.monotonic()
            peers = [(path := directory / f'counts-{index}.json', spawn(cache, path)) for index in range(3)]
            assert server.ready.wait(15), 'native clients never reached the loopback server'
            assert all(child.poll() is None for _, child in peers), 'peer exited before shared transfer began'
            server.release.set()
            counts = [complete(child, path) for path, child in peers]
            result = record('concurrent', cache, counts, started)
            assert result['requestCount'] == 1 and result['httpPayloadBytes'] == size
            assert sum(value['downloadedBytes'] for value in counts) == size
            assert sorted(value['source'] for value in counts) == ['cache', 'cache', 'download']
            assert sum(value['reusedBytes'] for value in counts) == 2 * size
            assert len(list((cache / 'downloads').glob('*.artifact'))) == 1
        return report
    finally:
        server.release.set()
        for child in processes:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
                try: child.communicate(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL); child.communicate(timeout=5)
        server.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--image', action='append', required=True, metavar='LABEL=PATH')
    parser.add_argument('--partial-bytes', type=int, default=64 * 1024**2)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    assert args.partial_bytes > 0
    images = []
    for item in args.image:
        label, path = item.split('=', 1)
        assert label and all(c.isascii() and (c.isalnum() or c == '-') for c in label)
        assert label not in [existing for existing, _ in images]
        images.append((label, Path(path).resolve()))
    binary = args.binary.resolve()
    evidence = {'schemaVersion': 1, 'measuredAt': datetime.now(timezone.utc).isoformat(),
        'candidateBinarySHA256': digest(binary),
        'candidateVersion': subprocess.check_output([binary, '--version'], text=True).strip(),
        'platform': platform.platform(), 'scope': 'private native acquire only; no bootstrap, installer, or VM',
        'networkAccounting': 'HTTP response payload bytes; excludes HTTP/TLS framing',
        'storageAccounting': 'logical size and st_blocks*512; not exclusive physical APFS allocation',
        'sourceBinding': 'source hashes describe the observed checkout; supplied binary build provenance is external',
        'sourceSHA256': {str(path.relative_to(ROOT)): digest(path) for path in
            [Path(__file__), ROOT / 'control/install_support/download.rs', ROOT / 'control/install_support/manifest.rs',
             ROOT / 'control/install_support/upgrade.rs']}, 'artifacts': []}
    with tempfile.TemporaryDirectory(prefix='hamn-acquire-measure-', dir='/tmp') as temporary:
        root = Path(temporary).resolve()
        cert, key = certificate(root)
        for label, image in images:
            evidence['artifacts'].append(measure(binary, image, label, root, cert, key, args.partial_bytes))
            args.output.write_text(json.dumps(evidence, indent=2) + '\n')
    print(json.dumps({'evidence': str(args.output.resolve()), 'candidateBinarySHA256': evidence['candidateBinarySHA256'],
                      'artifacts': [{key: item[key] for key in ('label', 'imageSHA256', 'compressedBytes')}
                                    for item in evidence['artifacts']]}, indent=2))


if __name__ == '__main__': main()
