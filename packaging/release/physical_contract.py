"""Required evidence for promotion of the exact single-binary release candidate."""
import hashlib
import json
import re
from pathlib import Path

CHECKS = {
    'singleBinary', 'vmLifecycle', 'multipleProfiles', 'dockerWithoutCli',
    'dockerApi', 'externalDockerSocket', 'externalKubernetes', 'tuiTerminalRestore',
    'vmSurvivesTuiExit', 'k3sRunningRetirement', 'k3sStoppedRetirement',
    'dockerDataPreserved', 'kubeconfigUnchanged', 'cleanup',
}


def sha256(path):
    with Path(path).open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def read_json(path):
    path = Path(path)
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 2 * 1024 * 1024:
        raise ValueError('unsafe or excessive evidence file')
    return json.loads(path.read_bytes())


def validate_candidate(directory, tag, commit, tree):
    directory = Path(directory)
    value = read_json(directory / 'candidate.json')
    if set(value) != {'schemaVersion', 'kind', 'tag', 'version', 'commit', 'sourceTree', 'artifacts'} \
            or value['schemaVersion'] != 1 or value['kind'] != 'hamn-release-candidate':
        raise ValueError('candidate schema is invalid')
    if not re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+-rc\.[0-9]+', tag) \
            or value['tag'] != tag or value['version'] != tag.rsplit('-rc.', 1)[0] \
            or value['commit'] != commit or value['sourceTree'] != tree:
        raise ValueError('candidate source identity mismatch')
    expected = {'hamn-' + value['version'] + suffix for suffix in
                ['-darwin-arm64.tar.gz', '-ubuntu-24.04-arm64.img', '.spdx.json']} | {'install.sh'}
    entries = value['artifacts']
    if not isinstance(entries, list) or len(entries) != 4 or any(not isinstance(item, dict) or set(item) != {'name', 'sha256'} for item in entries):
        raise ValueError('candidate artifact schema is invalid')
    artifacts = {item['name']: item['sha256'] for item in entries}
    if set(artifacts) != expected or {p.name for p in directory.iterdir()} != expected | {'candidate.json', 'SHA256SUMS'}:
        raise ValueError('candidate artifact set is invalid')
    checksums = {}
    path = directory / 'SHA256SUMS'
    if path.is_symlink() or path.stat().st_size > 4096:
        raise ValueError('unsafe checksums file')
    for line in path.read_text().splitlines():
        match = re.fullmatch(r'([0-9a-f]{64})  ([A-Za-z0-9._-]+)', line)
        if not match or match[2] in checksums:
            raise ValueError('invalid candidate checksums')
        checksums[match[2]] = match[1]
    if checksums != dict(artifacts, **{'candidate.json': sha256(directory / 'candidate.json')}):
        raise ValueError('candidate checksums binding mismatch')
    for name, digest in artifacts.items():
        path = directory / name
        if path.is_symlink() or not path.is_file() or sha256(path) != digest:
            raise ValueError('candidate artifact digest mismatch')
    return value


def validate(candidate_path, checksums_path, evidence_path, run, attempt):
    candidate = read_json(candidate_path)
    evidence = read_json(evidence_path)
    if set(evidence) != {'schemaVersion', 'kind', 'validationMode', 'tag', 'commit', 'sourceTree',
                         'workflow', 'candidate', 'checks', 'legacy', 'kubernetes'}:
        raise ValueError('physical validation evidence schema is invalid')
    if evidence['schemaVersion'] != 2 or evidence['kind'] != 'hamn-physical-validation-evidence' \
            or evidence['validationMode'] != 'physical-apple-silicon':
        raise ValueError('physical validation identity is invalid')
    for key in ['tag', 'commit', 'sourceTree']:
        if evidence[key] != candidate[key]:
            raise ValueError('physical validation provenance mismatch')
    if evidence['workflow'] != {'run': run, 'attempt': attempt}:
        raise ValueError('physical validation workflow mismatch')
    artifacts = {entry['name']: entry['sha256'] for entry in candidate['artifacts']}
    if evidence['candidate'] != {'candidateJsonSha256': sha256(candidate_path),
                                 'checksumsSha256': sha256(checksums_path), 'artifacts': artifacts}:
        raise ValueError('physical validation candidate binding mismatch')
    checks = evidence['checks']
    if not isinstance(checks, dict) or set(checks) != CHECKS or any(value is not True for value in checks.values()):
        raise ValueError('physical validation checks are incomplete')
    legacy = evidence['legacy']
    if not isinstance(legacy, dict) or set(legacy) != {'running', 'stopped'}:
        raise ValueError('both legacy K3s states require evidence')
    for state in legacy.values():
        if not isinstance(state, dict) or set(state) != {'before', 'after', 'k3sRemoved', 'journalComplete', 'sourceSha256'}:
            raise ValueError('legacy retirement evidence schema is invalid')
        if state['before'] != state['after'] or state['k3sRemoved'] is not True or state['journalComplete'] is not True:
            raise ValueError('legacy retirement did not preserve Docker data')
        snapshot = state['before']
        if not isinstance(snapshot, dict) or set(snapshot) != {'containers', 'images', 'volumes', 'networks', 'builtinNetworks', 'volumeSha256'}:
            raise ValueError('Docker preservation snapshot is invalid')
        if snapshot['builtinNetworks'] != ['bridge', 'host', 'none']:
            raise ValueError('Docker built-in networks were not preserved')
        for group in ['containers', 'images', 'volumes', 'networks']:
            values = snapshot[group]
            if not isinstance(values, list) or not values or any(not isinstance(value, str) or not value for value in values):
                raise ValueError('Docker preservation requires nonempty object identifiers')
        for value in [snapshot['volumeSha256'], state['sourceSha256']]:
            if not isinstance(value, str) or len(value) != 64 or any(c not in '0123456789abcdef' for c in value):
                raise ValueError('invalid legacy fixture or volume digest')
    kube = evidence['kubernetes']
    if not isinstance(kube, dict) or kube.get('passed') is not True or kube.get('namespaceRemoved') is not True \
            or kube.get('kubeconfigUnchanged') is not True or kube.get('kind') != 'hamn-external-kubernetes-e2e':
        raise ValueError('external Kubernetes evidence is incomplete')
    return evidence
