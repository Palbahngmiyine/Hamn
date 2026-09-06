#!/usr/bin/env python3
"""Defer new release notes until the manifest's previous tag is published."""
import json
import os
from pathlib import Path
import re
import subprocess


def ready(repository, manifest):
    if not re.fullmatch(r'[\w.-]+/[\w.-]+', repository) or not isinstance(manifest, dict):
        raise ValueError('invalid release identity')
    version = manifest.get('.')
    if set(manifest) != {'.'} or not isinstance(version, str) or not re.fullmatch(
            r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', version):
        raise ValueError('invalid release version')
    tag = 'v' + version
    result = subprocess.run(['gh', 'api', '--include',
        f'repos/{repository}/releases/tags/{tag}'], capture_output=True, text=True,
        timeout=30)
    headers, separator, body = result.stdout.partition('\n\n')
    status = re.match(r'HTTP/\S+ (\d{3})(?: |$)', headers)
    if not separator or not status:
        raise ValueError('release API returned an invalid HTTP response')
    if status[1] == '404' and result.returncode == 1:
        return False
    result.check_returncode()
    if status[1] != '200':
        raise ValueError('release API returned an unexpected status')
    release = json.loads(body)
    if not isinstance(release, dict) or release.get('tag_name') != tag or \
            type(release.get('draft')) is not bool:
        raise ValueError('release API returned an invalid release')
    if release['draft']:
        return False
    if release.get('prerelease') is not False or release.get('immutable') is not True:
        raise ValueError('current release must be stable and immutable')
    return True


if __name__ == '__main__':
    manifest = json.loads(Path('.release-please-manifest.json').read_text())
    published = ready(os.environ['GITHUB_REPOSITORY'], manifest)
    with open(os.environ['GITHUB_OUTPUT'], 'a', encoding='utf-8') as output:
        output.write(f'ready={str(published).lower()}\n')
    print('Current release is published.' if published else
          'Release publication is pending; defer release notes until Release succeeds.')
