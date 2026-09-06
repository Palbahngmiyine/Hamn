#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('completion',
    Path(__file__).resolve().parents[2] / 'packaging/release/complete-release-pr.py')
completion = importlib.util.module_from_spec(spec)
spec.loader.exec_module(completion)
COMMIT, MERGE = 'a' * 40, 'b' * 40


class Completion(unittest.TestCase):
    def exercise(self, release=None, version='0.1.0', ancestor=0, pending=None):
        if release is None:
            release = dict(tagName='v0.1.0', targetCommitish=COMMIT,
                           isDraft=False, isPrerelease=False, isImmutable=True)
        if pending is None:
            pending = [{'number': 43, 'mergeCommit': {'oid': MERGE}}]
        edits = []

        def output(*args):
            if args[:3] == ('gh', 'release', 'view'):
                return json.dumps(release)
            if args[:3] == ('gh', 'pr', 'list'):
                return json.dumps(pending)
            if args[:2] == ('git', 'show'):
                self.assertEqual(args[2], MERGE + ':.release-please-manifest.json')
                return json.dumps({'.': version})
            if args[:3] == ('gh', 'pr', 'edit'):
                edits.append(args)
                return ''
            raise AssertionError(args)

        with patch.object(completion, 'output', side_effect=output), patch.object(
                completion.subprocess, 'run', return_value=subprocess.CompletedProcess([], ancestor)):
            completion.complete('example/hamn', 'v0.1.0', COMMIT)
        return edits

    def test_published_ancestor_completes_only_pending_label(self):
        self.assertEqual(self.exercise(), [('gh', 'pr', 'edit', '43', '--repo',
            'example/hamn', '--remove-label', 'autorelease: pending')])

    def test_wrong_version_unrelated_commit_and_already_completed_are_untouched(self):
        self.assertEqual(self.exercise(version='0.2.0'), [])
        self.assertEqual(self.exercise(ancestor=1), [])
        self.assertEqual(self.exercise(pending=[]), [])

    def test_unpublished_mutable_wrong_tag_or_wrong_commit_cannot_complete(self):
        for key, value in [('isDraft', True), ('isPrerelease', True), ('isImmutable', False),
                           ('tagName', 'v0.2.0'), ('targetCommitish', MERGE)]:
            release = dict(tagName='v0.1.0', targetCommitish=COMMIT,
                           isDraft=False, isPrerelease=False, isImmutable=True)
            release[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.exercise(release=release)

    def test_git_failure_does_not_clear_pending(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.exercise(ancestor=128)


if __name__ == '__main__':
    unittest.main()
