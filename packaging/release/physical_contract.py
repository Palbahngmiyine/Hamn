"""Required evidence for promotion of the exact single-binary release candidate."""
import hashlib
import json
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
        if not isinstance(snapshot, dict) or set(snapshot) != {'containers', 'images', 'volumes', 'networks', 'volumeSha256'}:
            raise ValueError('Docker preservation snapshot is invalid')
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
