#!/usr/bin/env python3
"""Environment rows cannot reuse VM operations from a previous panel."""
import os
from test_tui_native_regressions import Harness


def close_input(harness, marker):
    os.write(harness.master, b'\x1b')
    harness.wait(lambda: marker not in harness.screen.text())


def check_environment_actions(kind):
    harness = Harness('containers')
    try:
        harness.until('old-target-row')
        # The owned, stopped profile and recorded Docker context deliberately
        # share a name. No VM is started and no mutation is ever confirmed.
        profile = harness.root / '.hamn/external'
        profile.mkdir(mode=0o700)
        config = profile / 'config.yaml'
        config.write_text('cpus: 2\nmemoryMiB: 2048\ndiskGiB: 60\n')
        config.chmod(0o600)
        original = config.read_bytes()
        (harness.root / 'released').touch()
        harness.send(b'v', 'vm list')
        harness.until('external')
        harness.send(b'e', 'Container environments')
        harness.wait(lambda: '[loading]' not in harness.screen.text())
        harness.until('external')
        if kind == 'docker':
            harness.send(b'j:ROW_BARRIER', ':ROW_BARRIER')
            close_input(harness, ':ROW_BARRIER')
        for key in b'tsdrgl':
            marker = ':ACTION_BARRIER'
            os.write(harness.master, bytes([key]) + marker.encode())
            harness.wait(lambda: marker in harness.screen.text() or 'Confirm vm' in harness.screen.text())
            assert 'Confirm vm' not in harness.screen.text(), harness.screen.text()
            assert 'Container environments' in harness.screen.text(), harness.screen.text()
            close_input(harness, marker)
            harness.until('Select an environment with Enter')
        for key in (b'c', b'v'):
            harness.send(key + b':PANEL_BARRIER', ':PANEL_BARRIER')
            assert 'vm configure' not in harness.screen.text(), harness.screen.text()
            assert 'Container environments' in harness.screen.text(), harness.screen.text()
            close_input(harness, ':PANEL_BARRIER')
        assert 'external  Hamn profile' in harness.screen.text(), harness.screen.text()
        assert 'external  Docker context' in harness.screen.text(), harness.screen.text()
        assert 'unix:///external/docker.sock' in harness.screen.text(), harness.screen.text()
        assert 't stop' not in harness.screen.text()
        assert 'v VM settings' not in harness.screen.text()
        harness.send(b'\r', 'new-target-row' if kind == 'docker' else 'old-target-row')
        harness.wait(lambda: '[loading]' not in harness.screen.text())
        if kind == 'docker':
            assert '--context external' in harness.screen.text()
            # request.words still contains vm list, but native external browsing
            # must not enable the configure shortcut either.
            harness.send(b'c:EXTERNAL_BARRIER', ':EXTERNAL_BARRIER')
            assert 'vm configure' not in harness.screen.text(), harness.screen.text()
            assert 'v VM settings' not in harness.screen.text()
            close_input(harness, ':EXTERNAL_BARRIER')
        else:
            harness.send(b'v', 'vm list')
            harness.until('external')
            harness.wait(lambda: '[loading]' not in harness.screen.text())
            harness.send(b'c', 'vm configure --profile external --cpu 2 --memory 2 --disk 60')
            close_input(harness, 'vm configure')
        assert config.read_bytes() == original
        assert sorted(p.name for p in profile.iterdir()) == ['config.yaml']
        assert not any('start' in args or 'stop' in args or 'rm' in args for _, args in harness.calls())
    finally:
        harness.close()


if __name__ == '__main__':
    for kind in ('docker', 'hamn'):
        check_environment_actions(kind)
    print('PASS: colliding Docker/Hamn names cannot dispatch VM actions in the environment picker; VM configure remains available only in a real Hamn VM panel')
