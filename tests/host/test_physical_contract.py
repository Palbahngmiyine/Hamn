#!/usr/bin/env python3
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('contract', 'packaging/release/physical_contract.py')
contract = importlib.util.module_from_spec(spec)
spec.loader.exec_module(contract)


class PhysicalEvidence(unittest.TestCase):
    def test_missing_false_foreign_and_changed_preservation_evidence_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            candidate = {'tag': 'v1.0.0-rc.1', 'commit': 'a' * 40, 'sourceTree': 'b' * 40,
                         'artifacts': [{'name': 'host.tar.gz', 'sha256': 'c' * 64}]}
            (root / 'candidate').write_text(json.dumps(candidate))
            (root / 'checksums').write_text('fixture checksums')
            snapshot = {key: ['owned-object'] for key in ['containers', 'images', 'volumes', 'networks']}
            snapshot['volumeSha256'] = 'e' * 64
            legacy = {'before': snapshot, 'after': snapshot, 'k3sRemoved': True, 'journalComplete': True, 'sourceSha256': 'd' * 64}
            evidence = {'schemaVersion': 2, 'kind': 'hamn-physical-validation-evidence', 'validationMode': 'physical-apple-silicon',
                **{key: candidate[key] for key in ['tag', 'commit', 'sourceTree']},
                'workflow': {'run': '1', 'attempt': '2'}, 'checks': dict.fromkeys(contract.CHECKS, True),
                'candidate': {'candidateJsonSha256': contract.sha256(root / 'candidate'),
                              'checksumsSha256': contract.sha256(root / 'checksums'), 'artifacts': {'host.tar.gz': 'c' * 64}},
                'legacy': {'running': legacy, 'stopped': legacy},
                'kubernetes': {'kind': 'hamn-external-kubernetes-e2e', 'passed': True, 'namespaceRemoved': True, 'kubeconfigUnchanged': True}}
            def verify(value):
                (root / 'evidence').write_text(json.dumps(value))
                return contract.validate(root / 'candidate', root / 'checksums', root / 'evidence', '1', '2')
            verify(evidence)
            for key in contract.CHECKS:
                changed = copy.deepcopy(evidence)
                changed['checks'][key] = False
                with self.assertRaises(ValueError): verify(changed)
                del changed['checks'][key]
                with self.assertRaises(ValueError): verify(changed)
            for section, key, value in [('workflow', 'run', '3'), ('candidate', 'candidateJsonSha256', '0' * 64),
                                         ('kubernetes', 'namespaceRemoved', False)]:
                changed = copy.deepcopy(evidence)
                changed[section][key] = value
                with self.assertRaises(ValueError): verify(changed)
            changed = copy.deepcopy(evidence)
            changed['legacy']['running']['after'] = dict(snapshot, volumeSha256='0' * 64)
            with self.assertRaises(ValueError): verify(changed)
            changed = copy.deepcopy(evidence)
            del changed['legacy']['stopped']
            with self.assertRaises(ValueError): verify(changed)


if __name__ == '__main__':
    unittest.main()
