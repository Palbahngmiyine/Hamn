#!/usr/bin/env python3
"""Clear pending release PRs only after their exact version is published."""
import json
import os
import re
import subprocess
import sys


def output(*args):
    return subprocess.check_output(args, text=True)


def complete(repository, tag, commit):
    if not re.fullmatch(r'[\w.-]+/[\w.-]+', repository) or not re.fullmatch(
            r'v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', tag) or not re.fullmatch(
            r'[0-9a-f]{40}', commit):
        raise ValueError('invalid release identity')
    release = json.loads(output('gh', 'release', 'view', tag, '--repo', repository,
        '--json', 'tagName,targetCommitish,isDraft,isPrerelease,isImmutable'))
    if release != dict(tagName=tag, targetCommitish=commit, isDraft=False,
                       isPrerelease=False, isImmutable=True):
        raise ValueError('release must be immutable, published, and bound to this commit')
    pending = json.loads(output('gh', 'pr', 'list', '--repo', repository, '--state',
        'merged', '--base', 'main', '--label', 'autorelease: pending', '--limit', '1000',
        '--json', 'number,mergeCommit'))
    if not isinstance(pending, list) or len(pending) >= 1000:
        raise ValueError('pending release PR listing is incomplete')
    for pr in pending:
        sha = pr['mergeCommit']['oid']
        number = pr['number']
        if type(number) is not int or number <= 0 or not re.fullmatch(r'[0-9a-f]{40}', sha):
            raise ValueError('invalid release PR identity')
        ancestor = subprocess.run(['git', 'merge-base', '--is-ancestor', sha, commit])
        if ancestor.returncode == 1:
            continue
        ancestor.check_returncode()
        manifest = json.loads(output('git', 'show', sha + ':.release-please-manifest.json'))
        if manifest != {'.': tag[1:]}:
            continue
        # Removing only this label preserves unrelated labels and is idempotent.
        output('gh', 'pr', 'edit', str(number), '--repo', repository,
               '--remove-label', 'autorelease: pending')
        print(f'Release PR #{number} completed for {tag}')


if __name__ == '__main__':
    complete(os.environ['GITHUB_REPOSITORY'], sys.argv[1], sys.argv[2])
