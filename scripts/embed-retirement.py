#!/usr/bin/env python3
"""Build a fixed migration payload; the runtime never loads source-tree files."""
import hashlib
import json
from pathlib import Path
import sys

root = Path(__file__).resolve().parent.parent
unit = root / 'host/migration/legacy-k3s.service'
payload = {'unitSha256': hashlib.sha256(unit.read_bytes()).hexdigest(),
           'helpers': {name: (root / f'guest/scripts/{name}.sh').read_text()
                       for name in ('verify-image-contract', 'guest-deployment-transaction')}}
script = (root / 'host/migration/retire_k3s.py').read_text()
script += '\nmigrate(json.loads(' + repr(json.dumps(payload)) + '))\n'
# The SSH command argument stays well below macOS ARG_MAX including quoting.
assert len(script.encode()) < 100000
text = '/* Generated fixed retirement payload. */\nstatic const char retirement_payload[] =\n'
text += '\n'.join(json.dumps(line, ensure_ascii=True) for line in script.splitlines(keepends=True))
text += ';\n'
output = Path(sys.argv[1])
output.parent.mkdir(parents=True, exist_ok=True)
if not output.exists() or output.read_text() != text:
    temporary = output.with_suffix('.tmp')
    temporary.write_text(text)
    temporary.replace(output)
