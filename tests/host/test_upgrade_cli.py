#!/usr/bin/env python3
"""Canonical aliases, read-only checks and guest-only repair on real installs."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import selectors
import signal
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[2]
HAMN = (ROOT / os.environ.get("HAMN", "build/hamn")).resolve()


def digest(path): return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    version = subprocess.check_output([HAMN, "--version"], text=True).strip().split()[1]
    source_check=subprocess.run([HAMN,"upgrade","--check","--manifest","https://unreachable.invalid/manifest","--output","json"],capture_output=True,text=True,timeout=10)
    assert source_check.returncode==0,source_check.stderr
    assert json.loads(source_check.stdout)["status"]=="unsupported-install"
    assert json.loads(source_check.stdout)["downloadedBytes"]==0
    with tempfile.TemporaryDirectory(prefix="hamn-upgrade-cli-") as directory:
        root = Path(directory).resolve()
        home = root / "home"; home.mkdir()
        bindir, datadir = home / "bin", home / "source"
        subprocess.run(["bash", ROOT / "scripts/install-host.sh", HAMN, bindir, datadir], check=True, capture_output=True)
        command = bindir / "hamn"
        original = os.readlink(command)
        release = root / "release"; (release / "bin").mkdir(parents=True)
        shutil.copy2(HAMN, release / "bin/hamn")
        for name in ("scripts", "packaging"): shutil.copytree(ROOT / name, release / name)
        (release / "packaging/release/update-manifest-url").write_text("https://example.invalid/manifest-v3.json\n")
        archive = root / "host.tar.gz"
        with tarfile.open(archive, "w:gz") as bundle: bundle.add(release, arcname="release")
        guest = root / "guest.img"; guest.write_bytes(b"guest-only repair fixture\n")
        value = {"schemaVersion":3,"channel":"stable","version":"v"+version,"commit":"1"*40,
            "validationMode":"github-hosted-no-vm","compatibility":{"os":"darwin","architecture":"arm64","minimumMacOS":"13.0"},
            "artifacts":{name:{"url":"file://"+str(path),"sha256":digest(path),"size":path.stat().st_size}
                for name,path in (("host",archive),("guestImage",guest))}}
        value["artifacts"]["guestImage"].update(format="qcow2",compression="zlib",virtualSize=8*1024**3)
        manifest=root / "manifest.json"; manifest.write_text(json.dumps(value))
        env={**os.environ,"HOME":str(home),"TMPDIR":str(root),"HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS":"1"}
        def run(*args, success=True):
            result=subprocess.run([command,*args,"--manifest",manifest], env=env,capture_output=True,text=True,timeout=60)
            assert (result.returncode==0)==success,(args,result.returncode,result.stdout,result.stderr)
            return result
        checked=run("upgrade","--check","--output","json")
        assert json.loads(checked.stdout)["status"]=="repair-required",checked.stdout
        assert os.readlink(command)==original and not (home / ".hamn").exists()
        installed=json.loads(run("upgrade","--output","json").stdout)
        assert installed["status"]=="updated" and installed["profileDisksChanged"] is False
        active=os.readlink(command)
        assert active!=original
        profile=home / ".hamn/owned-profile"; profile.mkdir()
        disk=profile / "disk.img"; disk.write_bytes(b"must remain unchanged")
        disk_before=digest(disk)
        repeated=json.loads(run("update","--output","json").stdout)
        assert repeated["status"]=="up-to-date" and repeated["downloadedBytes"]==0
        assert repeated["reusedBytes"]==archive.stat().st_size+guest.stat().st_size
        assert os.readlink(command)==active
        selection=home / ".hamn/cache/guest-image.json"
        cached=selection.parent / json.loads(selection.read_text())["file"]
        cached.write_bytes(b"damaged guest image")
        repaired=json.loads(run("upgrade","--output","json").stdout)
        assert repaired["status"]=="repaired" and repaired["artifacts"]["host"]["downloadedBytes"]==0
        assert os.readlink(command)==active and cached.read_bytes()==guest.read_bytes()
        selection.unlink()
        repaired=json.loads(run("upgrade","--output","json").stdout)
        assert repaired["status"]=="repaired" and os.readlink(command)==active
        forced=json.loads(run("upgrade","--force","--output","json").stdout)
        assert forced["status"]=="updated" and forced["downloadedBytes"]==0
        assert os.readlink(command)!=active
        active=os.readlink(command)
        headless=json.loads(run("--headless","system","update","--check").stdout)
        assert headless["ok"] is True and headless["data"]["status"]=="up-to-date"
        run("upgrade","--check","--force",success=False)
        saved=manifest.read_bytes(); value["version"]="v0.0.0"; manifest.write_text(json.dumps(value))
        assert json.loads(run("upgrade","--check","--output","json").stdout)["status"]=="ahead"
        run("upgrade","--output","json",success=False)
        assert os.readlink(command)==active and digest(disk)==disk_before
        manifest.write_bytes(saved)
        direct=subprocess.run([active,"upgrade","--manifest",manifest],env=env,capture_output=True,timeout=10)
        assert direct.returncode!=0 and b"managed hamn command symlink" in direct.stderr
        direct_check=subprocess.run([active,"upgrade","--check","--manifest","https://unreachable.invalid/manifest","--output","json"],env=env,capture_output=True,text=True,timeout=10)
        assert direct_check.returncode==0 and json.loads(direct_check.stdout)["status"]=="unsupported-install",direct_check.stderr
        helper=Path(active).parent.parent / "share/hamn/src/scripts/update-host.sh"
        # Selection-only journal interruption matrix, including recovery itself
        # followed by a malformed manifest. No host pointer may be rewritten.
        for point in ("PREPARED", "AFTER_GUEST_SELECTION"):
            for termination in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT, signal.SIGKILL):
                selection.unlink(missing_ok=True)
                ready=root / "ready"; release_fifo=root / "release-fifo"
                os.mkfifo(ready); os.mkfifo(release_fifo)
                ready_fd=os.open(ready,os.O_RDWR|os.O_NONBLOCK)
                child=None
                try:
                    barrier_env={**env,f"HAMN_TEST_UPDATE_{point}_READY_FIFO":str(ready),f"HAMN_TEST_UPDATE_{point}_RELEASE_FIFO":str(release_fifo)}
                    child=subprocess.Popen(["bash",helper,"--bindir",bindir,"--datadir",datadir,"--manifest",manifest],
                        env=barrier_env,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
                    with selectors.DefaultSelector() as selector:
                        selector.register(ready_fd,selectors.EVENT_READ)
                        assert selector.select(30),(point,termination,"barrier not reached",child.poll())
                    assert os.read(ready_fd,64)==b"ready\n"
                    journal=home / ".hamn/cache/.hamn-update-transaction"
                    state=(journal / "state").read_text()
                    assert state.startswith("version=2\n") and "hostMutation=0" in state
                    child.send_signal(termination)
                    stdout,stderr=child.communicate(timeout=15)
                    assert child.returncode!=0,(point,termination,stdout,stderr)
                    assert os.readlink(command)==active
                    if termination==signal.SIGKILL:
                        assert journal.is_dir()
                        saved_manifest=manifest.read_bytes(); manifest.write_text("{")
                        run("upgrade","--output","json",success=False)
                        manifest.write_bytes(saved_manifest)
                    assert not journal.exists() and not selection.exists(),(point,termination,stderr)
                    run("upgrade","--output","json")
                finally:
                    if child is not None and child.poll() is None: child.kill(); child.communicate(timeout=5)
                    os.close(ready_fd); ready.unlink(); release_fifo.unlink()
        # Frontend cancellation must wait for its owned worker/helper rollback,
        # not merely kill the Rust worker and leave its installer running.
        for operation in (("upgrade","--force","--output","json"), ("--headless","system","update","--yes","--force")):
            ready=root / "frontend-ready"; release_fifo=root / "frontend-release"
            os.mkfifo(ready); os.mkfifo(release_fifo)
            ready_fd=os.open(ready,os.O_RDWR|os.O_NONBLOCK)
            saved_selection=selection.read_bytes()
            child=None
            try:
                barrier_env={**env,"HAMN_TEST_UPDATE_AFTER_HOST_INSTALL_READY_FIFO":str(ready),"HAMN_TEST_UPDATE_AFTER_HOST_INSTALL_RELEASE_FIFO":str(release_fifo)}
                child=subprocess.Popen([command,*operation,"--manifest",manifest],env=barrier_env,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
                with selectors.DefaultSelector() as selector:
                    selector.register(ready_fd,selectors.EVENT_READ)
                    assert selector.select(30),(operation,"frontend barrier not reached",child.poll())
                assert os.read(ready_fd,64)==b"ready\n"
                child.send_signal(signal.SIGTERM)
                stdout,stderr=child.communicate(timeout=20)
                assert child.returncode!=0,(operation,stdout,stderr)
                assert os.readlink(command)==active and selection.read_bytes()==saved_selection,(operation,stderr)
                assert not (home / ".hamn/cache/.hamn-update-transaction").exists(),(operation,stderr)
                assert digest(disk)==disk_before
            finally:
                if child is not None and child.poll() is None: child.kill(); child.communicate(timeout=5)
                os.close(ready_fd); ready.unlink(); release_fifo.unlink()
        # Recover an old v1 journal before validating a new (invalid) manifest.
        journal=home / ".hamn/cache/.hamn-update-transaction"; journal.mkdir(mode=0o700)
        saved_selection=selection.read_bytes()
        for name, contents in {"state":b"version=1\nbootstrap=0\nselection=present\n",
                "attempt":b"Abc123\n","old-target":(active+"\n").encode(),
                "previous-selection":saved_selection,"new-selection":b"{}\n"}.items():
            path=journal / name; path.write_bytes(contents); path.chmod(0o600)
        selection.write_bytes(b"{}\n")
        saved_manifest=manifest.read_bytes(); manifest.write_text("{")
        run("upgrade","--output","json",success=False)
        manifest.write_bytes(saved_manifest)
        assert not journal.exists() and selection.read_bytes()==saved_selection and os.readlink(command)==active
        # A pending v1 journal is not touched by a manifest-only check.
        journal=home / ".hamn/cache/.hamn-update-transaction"; journal.mkdir(mode=0o700)
        sentinel=journal / "state"; sentinel.write_text("version=1\npartial fixture\n"); sentinel.chmod(0o600)
        before=sentinel.read_bytes()
        run("upgrade","--check","--output","json")
        assert sentinel.read_bytes()==before and os.readlink(command)==active and digest(disk)==disk_before
        print("PASS: upgrade/update aliases, immutable check, zero-payload no-op/force, guest-only repair, downgrade and direct-generation refusal")


if __name__=="__main__": main()
