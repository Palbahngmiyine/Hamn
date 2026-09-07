#!/usr/bin/env python3
"""The live restart check rejects desired-state changes, not controller revisions."""
import copy
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'packaging/release'))
from workspace_live_kubernetes import assert_deployment_preserved


class PreservationAssertions(unittest.TestCase):
    def setUp(self):
        self.before = {'metadata': {'uid': 'selected', 'resourceVersion': 'before',
            'annotations': {'guard-proof': 'changed'}}, 'spec': {'replicas': 0,
            'template': {'metadata': {'annotations': {'keep': 'preserved'}}}}}

    def test_controller_status_and_revision_do_not_imply_a_restart(self):
        after = copy.deepcopy(self.before)
        after['metadata']['resourceVersion'] = 'opaque-new-version'
        after['metadata']['annotations']['deployment.kubernetes.io/revision'] = '1'
        after['status'] = {'observedGeneration': 1}
        assert_deployment_preserved(self.before, after)

    def test_replacement_spec_change_or_lost_concurrent_edit_fails(self):
        for mutate in (
            lambda v: v['metadata'].update(uid='replacement'),
            lambda v: v['spec'].update(replicas=1),
            lambda v: v['spec']['template']['metadata']['annotations'].update({'kubectl.kubernetes.io/restartedAt': 'now'}),
            lambda v: v['spec']['template']['metadata']['annotations'].update(keep='lost'),
            lambda v: v['metadata']['annotations'].update({'guard-proof': 'lost'}),
        ):
            with self.subTest(mutation=mutate):
                after = copy.deepcopy(self.before)
                mutate(after)
                with self.assertRaises(AssertionError):
                    assert_deployment_preserved(self.before, after)


if __name__ == '__main__':
    unittest.main()
