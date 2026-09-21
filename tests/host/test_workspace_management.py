#!/usr/bin/env python3
"""Focused oracles/PTY controls for the opt-in live management acceptance test."""
import copy
import json
import os
import sys
import unittest

from workspace_live_management import OWNER, assert_identity, assert_relations, choose, query, same_process
from test_tui_native_regressions import Harness


class EvidenceAssertions(unittest.TestCase):
    def test_cleanup_rejects_replacement_or_changed_owner(self):
        value = {'metadata': {'uid': 'owned', 'labels': {OWNER: 'token'}}}
        assert_identity(value, 'owned', 'token')
        for uid, owner in [('replacement', 'token'), ('owned', 'foreign')]:
            changed = copy.deepcopy(value)
            changed['metadata'].update(uid=uid, labels={OWNER: owner})
            with self.assertRaises(AssertionError): assert_identity(changed, 'owned', 'token')

    def test_selector_and_uid_oracles_reject_unrelated_rows(self):
        pod, decoy, selected, stale = ({'metadata': {'uid': uid}} for uid in ('pod', 'decoy', 'selected', 'stale'))
        selected['involvedObject'] = {'uid': 'pod'}
        stale['involvedObject'] = {'uid': 'old-pod'}
        args = [pod, decoy, selected, stale]
        assert_relations({'items': [pod]}, {'items': [selected]}, *args)
        for pods, events in [([pod, decoy], [selected]), ([pod], [selected, stale]), ([], [selected]), ([pod], [])]:
            with self.assertRaises(AssertionError): assert_relations({'items': pods}, {'items': events}, *args)

    def test_reused_pid_is_not_treated_as_the_owned_cli(self):
        owned = {'pid': 123, 'parent': 100, 'group': 123, 'started': 'original', 'command': 'kubectl logs pod'}
        self.assertTrue(same_process(owned, dict(owned, parent=1)))
        for key, value in [('pid', 124), ('group', 124), ('started', 'reused'), ('command', 'unrelated')]:
            self.assertFalse(same_process(owned, dict(owned, **{key: value})))
        self.assertFalse(same_process(owned, None))


class TerminalAdapter:
    def __init__(self, harness): self.harness, self.screen = harness, harness.screen
    def wait(self, predicate): self.harness.wait(predicate)
    def until(self, marker): self.harness.until(marker)
    def send(self, keys, marker=None):
        os.write(self.harness.master, keys)
        if marker: self.until(marker)


PEER = r'''import json, os, sys
from pathlib import Path
args = sys.argv[1:]; root = Path(os.environ['FIXTURE_ROOT'])
with (root/'calls').open('a') as output: output.write(json.dumps(['kubectl', args])+'\n')
kind = args[args.index('get')+1]
objects = json.loads((root/'objects').read_text())
value = objects[kind]
if 'yaml' in args:
    print('uid: '+value['metadata']['uid'])
else:
    print(json.dumps({'items':[value]}))
'''


class ManagementPTY(unittest.TestCase):
    def test_live_menu_helpers_navigate_custom_inspect_and_relationships(self):
        harness = Harness('kubernetes')
        terminal = TerminalAdapter(harness)
        try:
            harness.until('old-target-row')
            row = lambda name, kind, uid: {'apiVersion': 'v1', 'kind': kind,
                'metadata': {'name': name, 'namespace': 'review', 'uid': uid, 'resourceVersion': '1'}}
            custom = row('read-only', 'ReviewProbe', 'custom-uid')
            deployment = row('web', 'Deployment', 'deployment-uid')
            deployment['spec'] = {'selector': {'matchLabels': {'app': 'selected'}}}
            pod = row('web-pod', 'Pod', 'pod-uid')
            event = row('selected-event', 'Event', 'event-uid')
            (harness.root/'objects').write_text(json.dumps({'probes.example.test': custom,
                'deployments': deployment, 'pods': pod, 'events': event}))
            (harness.root/'bin/kubectl').write_text(f'#!{sys.executable}\n'+PEER)
            query(terminal, 'probes.example.test', 'read-only', 'review')
            terminal.send(b'm', 'Resource actions')
            self.assertIn('inspect', terminal.screen.text())
            self.assertNotIn('delete', terminal.screen.text())
            choose(terminal, 'Resource actions', 'inspect')
            terminal.until('custom-uid'); terminal.until('Exit code 0'); terminal.send(b'\r')
            query(terminal, 'deployments', 'web', 'review')
            terminal.send(b'm', 'related-pods'); choose(terminal, 'Resource actions', 'related-pods')
            terminal.until('> web-pod'); terminal.wait(lambda: '[loading]' not in terminal.screen.text())
            terminal.send(b'm', 'related-events'); choose(terminal, 'Resource actions', 'related-events')
            terminal.until('selected-event')
            commands = [args for _, args in harness.calls()]
            self.assertTrue(any('--selector' in args and 'app=selected' in args for args in commands))
            self.assertTrue(any('--field-selector' in args and 'involvedObject.uid=pod-uid' in args for args in commands))
        finally:
            harness.close()


if __name__ == '__main__': unittest.main()
