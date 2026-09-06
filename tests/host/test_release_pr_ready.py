#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('publication',
    Path(__file__).resolve().parents[2] / 'packaging/release/release-pr-ready.py')
publication = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publication)


class Publication(unittest.TestCase):
    def response(self, status=200, code=0, **fields):
        release = dict(tag_name='v0.1.0', draft=False, prerelease=False, immutable=True)
        release.update(fields)
        return subprocess.CompletedProcess([], code,
            f'HTTP/2.0 {status} Status\nContent-Type: application/json\n\n' +
            json.dumps(release), '')

    def exercise(self, response):
        with patch.object(publication.subprocess, 'run', return_value=response) as run:
            result = publication.ready('example/hamn', {'.': '0.1.0'})
            run.assert_called_once_with(['gh', 'api', '--include',
                'repos/example/hamn/releases/tags/v0.1.0'], capture_output=True,
                text=True, timeout=30)
            return result

    def test_manifest_merge_defers_until_exact_release_is_published(self):
        self.assertFalse(self.exercise(self.response(status=404, code=1)))
        self.assertFalse(self.exercise(self.response(draft=True, immutable=False)))
        self.assertTrue(self.exercise(self.response()))

    def test_api_and_network_failures_do_not_look_like_pending_publication(self):
        for status in (401, 403, 429, 500, 503):
            with self.subTest(status=status), self.assertRaises(subprocess.CalledProcessError):
                self.exercise(self.response(status=status, code=1))
        with self.assertRaises(ValueError):
            self.exercise(subprocess.CompletedProcess([], 1, '', 'connection failed'))
        with patch.object(publication.subprocess, 'run', side_effect=
                subprocess.TimeoutExpired('gh', 30)), self.assertRaises(subprocess.TimeoutExpired):
            publication.ready('example/hamn', {'.': '0.1.0'})

    def test_invalid_or_wrong_release_is_rejected(self):
        for fields in ({'tag_name': 'v0.0.1'}, {'draft': None}, {'prerelease': True},
                       {'immutable': False}, {'immutable': None}):
            with self.subTest(fields=fields), self.assertRaises(ValueError):
                self.exercise(self.response(**fields))
        with self.assertRaises(ValueError):
            self.exercise(self.response(status=202))
        with self.assertRaises(ValueError):
            self.exercise(subprocess.CompletedProcess([], 0, 'HTTP/2.0 200 OK\n\n{', ''))

    def test_invalid_configuration_cannot_call_the_api(self):
        with patch.object(publication.subprocess, 'run') as run:
            for manifest in (None, {}, {'.': None}, {'.': '0.1.0-rc.1'}, {'.': '01.0.0'},
                             {'.': '0.1.0', 'other': '0.1.0'}, {'.': '../tag'}):
                with self.subTest(manifest=manifest), self.assertRaises(ValueError):
                    publication.ready('example/hamn', manifest)
            with self.assertRaises(ValueError):
                publication.ready('example/hamn/../../other', {'.': '0.1.0'})
            run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
