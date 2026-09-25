//! A fresh curl-style bootstrap accepts only immutable release metadata,
//! installs the exact candidate bytes through the candidate's own
//! executable, and preserves the installation on every refusal.
//!
//! Candidates are built from this checkout with `release build-candidate`
//! (which rebuilds build/hamn as v0.0.1; the version found is restored
//! afterwards); run from the repository root, without the hosted
//! workflow's identity. Every install runs `bash install.sh` with an owned
//! HOME, TMPDIR and system PATH, so no real `~/.local` or `~/.hamn` is
//! touched.
use crate::release::files::sha256_file;
use crate::runner::{self, case};
use crate::support::exec::output_within;
use crate::support::release_driver::{Outcome, git, outcome, private_bin, search_path};
use crate::support::released;
use crate::support::tmp::TempDir;
use crate::support::upgrade::{self, Output};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    let _restore = RestoreHost::capture();
    runner::run(
        "release-artifacts",
        "candidate artifacts bootstrap atomically from immutable release metadata",
        vec![
            case(
                "non_arm64_builders_and_foreign_manifest_urls_are_refused",
                non_arm64_builders_and_foreign_manifest_urls_are_refused,
            ),
            case(
                "canonical_candidate_embeds_the_latest_v3_manifest_url",
                canonical_candidate_embeds_the_latest_v3_manifest_url,
            ),
            case(
                "local_candidate_bootstraps_exact_bytes_and_refuses_unsafe_installs",
                local_candidate_bootstraps_exact_bytes_and_refuses_unsafe_installs,
            ),
            case("candidate_installer_migrates_a_released_install", candidate_installer_migrates_a_released_install),
        ],
        filters,
    )
}

const TAG: &str = "v0.0.1-rc.1";
const HOST: &str = "hamn-v0.0.1-darwin-arm64";
const SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const CANONICAL_REPOSITORY: &str = "example/hamn";
const BUILD: Duration = Duration::from_secs(3600);
const INSTALL: Duration = Duration::from_secs(300);

/// Rebuilds build/hamn at the version found before a candidate build
/// replaced it (nothing when there was no build/hamn).
struct RestoreHost(Option<String>);

impl RestoreHost {
    fn capture() -> Self {
        let version = Command::new("build/hamn")
            .arg("--version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .and_then(|text| text.split_whitespace().nth(1).map(str::to_owned));
        Self(version)
    }
}

impl Drop for RestoreHost {
    fn drop(&mut self) {
        if let Some(version) = &self.0 {
            let output = output_within(Command::new("make").args(["host", &format!("VERSION={version}")]), BUILD);
            if !output.status.success() {
                eprintln!("release-artifacts: cannot restore build/hamn {version}: {output:?}");
            }
        }
    }
}

/// `hamn-dev release build-candidate` in this checkout for `output`, with
/// our environment minus the hosted workflow's identity, plus `extra`.
fn build_candidate(guest: &Path, output: &Path, extra: &[(&str, &OsStr)]) -> Outcome {
    let release_ref = git(Path::new("."), &["rev-parse", "HEAD"]);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["release", "build-candidate"]).stdin(Stdio::null());
    for name in ["GITHUB_ACTIONS", "GITHUB_REPOSITORY", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT"] {
        command.env_remove(name);
    }
    command
        .env("RELEASE_REF", release_ref)
        .env("RELEASE_TAG", TAG)
        .env("OUTPUT_DIR", output)
        .env("HAMN_GUEST_IMAGE", guest)
        .env("HAMN_RELEASE_ALLOW_DIRTY", "1")
        .envs(extra.iter().copied());
    outcome(output_within(&mut command, BUILD))
}

fn guest_fixture(work: &Path) -> PathBuf {
    let guest = work.join("guest.img");
    fs::write(&guest, "preconfigured guest image fixture\n").unwrap();
    guest
}

fn non_arm64_builders_and_foreign_manifest_urls_are_refused() {
    let work = TempDir::new("hamn-release-artifacts-");
    let root = fs::canonicalize(work.path()).unwrap();
    let guest = guest_fixture(&root);
    // `uname -m` reports x86_64 from a fixture ahead of our PATH.
    let fake = private_bin(&root.join("fake"), &[], &["uname"]);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut directories: Vec<PathBuf> = vec![fake];
    directories.extend(std::env::split_paths(&path));
    let directories: Vec<&Path> = directories.iter().map(PathBuf::as_path).collect();
    let search = search_path(&directories);
    build_candidate(
        &guest,
        &root.join("non-arm64-candidate"),
        &[
            ("PATH", &search),
            ("HAMN_DEV_FIXTURE", OsStr::new("release-uname")),
            ("HAMN_TEST_MACHINE", OsStr::new("x86_64")),
        ],
    )
    .failed_with("release candidate must build on Apple Silicon arm64");
    assert!(!root.join("non-arm64-candidate").exists());
    build_candidate(
        &guest,
        &root.join("rejected-candidate"),
        &[
            ("GITHUB_REPOSITORY", OsStr::new(CANONICAL_REPOSITORY)),
            ("HAMN_RELEASE_MANIFEST_URL", OsStr::new("https://downloads.example.invalid/other/manifest.json")),
        ],
    )
    .failed_with("HAMN_RELEASE_MANIFEST_URL must match the canonical GitHub Release manifest URL");
}

/// The archive member `member` of the gzip tar `archive`, read by the
/// system tar.
fn archive_member(archive: &Path, member: &str) -> String {
    String::from_utf8(archive_bytes(archive, member)).unwrap()
}

/// `bash INSTALLER ARGS...` with exactly HOME, TMPDIR, PATH and `extra`.
fn bootstrap(installer: &Path, home: &Path, tmp: &Path, path: &OsStr, args: &[&str], extra: &[(&str, &str)]) -> Output {
    let mut command = Command::new("/bin/bash");
    command
        .arg(installer)
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("TMPDIR", tmp)
        .env("PATH", path)
        .envs(extra.iter().copied())
        .stdin(Stdio::null());
    upgrade::run(&mut command, INSTALL)
}

fn canonical_candidate_embeds_the_latest_v3_manifest_url() {
    let work = TempDir::new("hamn-release-artifacts-");
    let root = fs::canonicalize(work.path()).unwrap();
    let guest = guest_fixture(&root);
    let candidate = root.join("canonical-candidate");
    build_candidate(&guest, &candidate, &[("GITHUB_REPOSITORY", OsStr::new(CANONICAL_REPOSITORY))]).succeeded();
    let pointer =
        archive_member(&candidate.join(format!("{HOST}.tar.gz")), &format!("{HOST}/share/hamn/update-manifest-url"));
    assert_eq!(
        pointer,
        format!("https://github.com/{CANONICAL_REPOSITORY}/releases/latest/download/hamn-update-manifest-v3.json\n"),
        "a GitHub candidate did not embed the canonical stable manifest URL"
    );
    // The embedded version, through the generated installer's public
    // behavior (valid shell quoting may differ).
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    let help = bootstrap(&candidate.join("install.sh"), &home, &root, OsStr::new(SYSTEM_PATH), &["--help"], &[]);
    assert_eq!(help.returncode, 0, "{}", help.stderr());
    assert!(
        help.stdout().lines().any(|line| line == "Install Hamn 0.0.1 for Apple Silicon macOS."),
        "{}",
        help.stdout()
    );
}

/// An untracked file in the checkout's packaging directory, removed when
/// dropped; no candidate may include it.
struct UntrackedInput(PathBuf);

impl Drop for UntrackedInput {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// The SBOM's creation metadata is exactly the builder's, and its two
/// package checksums bind the host and guest artifacts.
fn assert_sbom_binds(sbom: &Path, host: &str, guest: &str) {
    let document: Value = serde_json::from_slice(&fs::read(sbom).unwrap()).unwrap();
    let info = document["creationInfo"].as_object().expect("SBOM creationInfo");
    let keys: Vec<&str> = info.keys().map(String::as_str).collect();
    assert_eq!(keys, ["created", "creators", "licenseListVersion"]);
    assert_eq!(info["creators"], json!(["Tool: hamn-release-candidate"]));
    let created = info["created"].as_str().expect("SBOM creation time").as_bytes();
    let shape = b"0000-00-00T00:00:00Z";
    assert!(
        created.len() == shape.len()
            && created.iter().zip(shape).all(|(byte, pattern)| if *pattern == b'0' {
                byte.is_ascii_digit()
            } else {
                byte == pattern
            }),
        "SBOM creation time is not a UTC timestamp: {}",
        String::from_utf8_lossy(created)
    );
    let packages = document["packages"].as_array().expect("SBOM packages");
    assert_eq!(packages.len(), 2);
    let sums: BTreeMap<&str, &Value> = packages
        .iter()
        .map(|package| (package["name"].as_str().expect("package name"), &package["checksums"]))
        .collect();
    assert_eq!(
        sums.get(format!("{HOST}.tar.gz").as_str()),
        Some(&&json!([{"algorithm": "SHA256", "checksumValue": host}]))
    );
    assert_eq!(
        sums.get("hamn-v0.0.1-ubuntu-24.04-arm64.img"),
        Some(&&json!([{"algorithm": "SHA256", "checksumValue": guest}]))
    );
}

fn local_candidate_bootstraps_exact_bytes_and_refuses_unsafe_installs() {
    let work = TempDir::new("hamn-release-artifacts-");
    let root = fs::canonicalize(work.path()).unwrap();
    let guest = guest_fixture(&root);
    let untracked = UntrackedInput(
        upgrade::checkout().join(format!("packaging/release/.artifact-fixture.{}.log", std::process::id())),
    );
    fs::write(&untracked.0, "untracked local input\n").unwrap();
    let candidate = root.join("candidate");
    let manifest_url = format!("file://{}", root.join("manifest.json").display());
    build_candidate(
        &guest,
        &candidate,
        &[("HAMN_RELEASE_MANIFEST_URL", OsStr::new(&manifest_url)), ("HAMN_RELEASE_ALLOW_LOCAL", OsStr::new("1"))],
    )
    .succeeded();

    // The host archive is exactly one generation payload: the executable
    // and its manifest pointer. No checkout file (tracked or untracked),
    // guest source, removed harness or shell script is shipped.
    let host_artifact = candidate.join(format!("{HOST}.tar.gz"));
    let guest_artifact = candidate.join("hamn-v0.0.1-ubuntu-24.04-arm64.img");
    let listed = Command::new("/usr/bin/tar").arg("-tzf").arg(&host_artifact).output().unwrap();
    assert!(listed.status.success());
    let members: BTreeSet<String> = String::from_utf8(listed.stdout).unwrap().lines().map(str::to_owned).collect();
    let expected: BTreeSet<String> =
        ["/", "/bin/", "/bin/hamn", "/share/", "/share/hamn/", "/share/hamn/update-manifest-url"]
            .iter()
            .map(|suffix| format!("{HOST}{suffix}"))
            .collect();
    assert_eq!(members, expected, "the host artifact is not exactly one generation payload");
    let untracked_name = untracked.0.file_name().unwrap().to_str().unwrap();
    assert!(
        !members.iter().any(|member| member.contains(untracked_name)),
        "the host artifact contains untracked local files"
    );
    assert_eq!(
        archive_member(&host_artifact, &format!("{HOST}/share/hamn/update-manifest-url")),
        format!("{manifest_url}\n")
    );
    let host_hash = sha256_file(&host_artifact).unwrap();
    let guest_hash = sha256_file(&guest_artifact).unwrap();
    assert_sbom_binds(&candidate.join("hamn-v0.0.1.spdx.json"), &host_hash, &guest_hash);
    let tree = git(Path::new("."), &["rev-parse", "HEAD^{tree}"]);
    assert!(
        fs::read_to_string(candidate.join("candidate.json")).unwrap().contains(&format!("\"sourceTree\":\"{tree}\""))
    );
    // The pointer's target: the release's own v3 manifest.
    let mut manifest = upgrade::manifest(
        "v0.0.1",
        &upgrade::Artifact::local(&host_artifact),
        &upgrade::Artifact::local(&guest_artifact),
    );
    manifest["commit"] = git(Path::new("."), &["rev-parse", "HEAD"]).into();
    upgrade::write_json(&root.join("manifest.json"), &manifest);

    let installer = candidate.join("install.sh");
    let home = root.join("home");
    let tmp = root.join("tmp");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&tmp).unwrap();
    let system = OsStr::new(SYSTEM_PATH);
    let local = [("HAMN_INSTALL_ALLOW_LOCAL_ARTIFACTS", "1")];
    let untouched = |home: &Path| !home.join(".local").exists() && !home.join(".hamn").exists();
    let help = bootstrap(&installer, &home, &tmp, system, &["--help"], &[]);
    assert!(help.stdout().lines().any(|line| line == "Update later with: hamn upgrade"), "{}", help.stdout());
    assert!(untouched(&home));
    let unknown = bootstrap(&installer, &home, &tmp, system, &["--unknown"], &[]);
    assert_ne!(unknown.returncode, 0, "bootstrap accepted an unknown option");
    assert!(untouched(&home));
    let refused = bootstrap(&installer, &home, &tmp, system, &[], &[]);
    assert_ne!(refused.returncode, 0, "bootstrap accepted local release input by default");
    assert!(refused.stderr().contains("local artifacts are disabled"), "{}", refused.stderr());

    let installed = bootstrap(&installer, &home, &tmp, system, &[], &[("SHELL", "/bin/zsh"), local[0]]);
    assert_eq!(installed.returncode, 0, "{}", installed.stderr());
    assert!(installed.stdout.is_empty(), "{}", installed.stdout());
    let stderr = installed.stderr();
    assert!(stderr.lines().any(|line| line == "Installed Hamn 0.0.1."), "{stderr}");
    // PATH advice names the caller's shell file and is ready to paste.
    assert!(
        stderr
            .lines()
            .any(|line| line == "  echo 'export PATH=\"$HOME/.local/bin:$PATH\"' >> ~/.zshrc && source ~/.zshrc"),
        "{stderr}"
    );
    for jargon in ["hamn update:", "hamn install:", "verified cache", "atomically"] {
        assert!(
            !stderr.contains(jargon),
            "installer output repeats internal step prefixes or jargon ({jargon}): {stderr}"
        );
    }
    let command = home.join(".local/bin/hamn");
    let version = upgrade::run(Command::new(&command).arg("--version").env("HOME", &home), INSTALL);
    assert_eq!(version.stdout(), "hamn 0.0.1\n");
    let cache = home.join(".hamn/cache");
    assert!(
        fs::read_to_string(cache.join("guest-image.json")).unwrap().contains(&format!("\"sha256\":\"{guest_hash}\""))
    );
    assert!(cache.join(format!("hamn-guest-{guest_hash}.img")).is_file());
    // The installed generation is the archive's payload: the executable
    // and the release's manifest pointer, with no scripts or sources.
    let target = fs::read_link(&command).unwrap();
    let generation = upgrade::generation_of(&target);
    assert_eq!(fs::read(&target).unwrap(), archive_bytes(&host_artifact, &format!("{HOST}/bin/hamn")));
    assert_eq!(
        fs::read_to_string(generation.join("share/hamn/update-manifest-url")).unwrap(),
        format!("{manifest_url}\n")
    );
    assert!(fs::symlink_metadata(generation.join("share/hamn/src")).is_err());

    let with_command = search_path(&[
        &home.join(".local/bin"),
        Path::new("/usr/bin"),
        Path::new("/bin"),
        Path::new("/usr/sbin"),
        Path::new("/sbin"),
    ]);
    let reinstall = bootstrap(&installer, &home, &tmp, &with_command, &[], &local);
    assert_eq!(reinstall.returncode, 0, "{}", reinstall.stderr());
    assert!(reinstall.stdout.is_empty());
    let stderr = reinstall.stderr();
    assert!(
        !stderr.contains("export PATH="),
        "bootstrap suggested PATH setup when the command directory was present: {stderr}"
    );
    assert!(stderr.lines().any(|line| line == "Hamn 0.0.1 is already installed."), "{stderr}");
    assert!(stderr.lines().any(|line| line == "Run hamn to get started. Update later with hamn upgrade."), "{stderr}");

    // Another hamn earlier on PATH is named, with advice for bash.
    let shadow = root.join("shadow");
    fs::create_dir(&shadow).unwrap();
    upgrade::write_executable(&shadow.join("hamn"), "#!/bin/sh\necho old-hamn\n");
    let shadowed_path = search_path(&[&shadow, &home.join(".local/bin"), Path::new("/usr/bin"), Path::new("/bin")]);
    let shadowed = bootstrap(&installer, &home, &tmp, &shadowed_path, &[], &[("SHELL", "/bin/bash"), local[0]]);
    let stderr = shadowed.stderr();
    assert!(
        stderr
            .lines()
            .any(|line| line == format!("Another hamn comes first on your PATH: {}", shadow.join("hamn").display())),
        "{stderr}"
    );
    assert!(stderr.contains("export PATH=\"$HOME/.local/bin:$PATH\"' >> ~/.bash_profile"), "{stderr}");
    assert!(
        !stderr.contains("Run hamn to get started. Update later"),
        "bootstrap reported a shadowed command ready: {stderr}"
    );
    assert_eq!(fs::read_link(&command).unwrap(), target);
    // An earlier alias symlink to the installed command is already usable.
    fs::remove_file(shadow.join("hamn")).unwrap();
    std::os::unix::fs::symlink(&command, shadow.join("hamn")).unwrap();
    let alias_path =
        search_path(&[&shadow, Path::new("/usr/bin"), Path::new("/bin"), Path::new("/usr/sbin"), Path::new("/sbin")]);
    let alias = bootstrap(&installer, &home, &tmp, &alias_path, &[], &local);
    let stderr = alias.stderr();
    assert!(stderr.lines().any(|line| line == "Run hamn to get started. Update later with hamn upgrade."), "{stderr}");
    assert!(!stderr.contains("export PATH="), "bootstrap rejected a PATH symlink to the installed command: {stderr}");

    // The empty ownership marker of a pre-release install is not migrated,
    // even when the release receipt matches; neither it nor the link changes.
    let data_marker = home.join(".local/share/hamn/src/.hamn-managed");
    fs::write(&data_marker, "").unwrap();
    let migrated = bootstrap(&installer, &home, &tmp, system, &[], &local);
    assert_ne!(migrated.returncode, 0, "bootstrap migrated an empty pre-release data marker");
    assert!(
        migrated.stderr().contains("is a pre-release Hamn install (empty data marker), which is no longer migrated"),
        "{}",
        migrated.stderr()
    );
    assert_eq!(fs::metadata(&data_marker).unwrap().len(), 0);
    assert_eq!(fs::read_link(&command).unwrap(), target);
    fs::write(&data_marker, "version=1\n").unwrap();

    // A generation marked as the Hamn 0.1.x layout that fails the checks
    // its installer applied (it has no share/hamn/src scripts) is refused
    // with reinstall advice before any download; nothing changes.
    let marker = generation.join(".hamn-generation");
    let marker_text = fs::read_to_string(&marker).unwrap();
    fs::write(&marker, marker_text.replacen("version=2", "version=1", 1)).unwrap();
    let selection = fs::read(cache.join("guest-image.json")).unwrap();
    let earlier = bootstrap(&installer, &home, &tmp, system, &[], &local);
    assert_ne!(earlier.returncode, 0, "bootstrap adopted an unverifiable 0.1.x generation");
    let stderr = earlier.stderr();
    assert!(
        stderr.contains("points to a Hamn 0.1.x generation that fails the ownership checks")
            && stderr.contains("then reinstall with install.sh"),
        "{stderr}"
    );
    assert_eq!(fs::read_link(&command).unwrap(), target);
    assert_eq!(fs::read(cache.join("guest-image.json")).unwrap(), selection);
    fs::write(&marker, &marker_text).unwrap();

    // A regular legacy or foreign executable is refused before any
    // unjournaled cutover or runtime state.
    let legacy_home = root.join("legacy-home");
    fs::create_dir_all(legacy_home.join(".local/bin")).unwrap();
    fs::copy(&target, legacy_home.join(".local/bin/hamn")).unwrap();
    let legacy_hash = sha256_file(&legacy_home.join(".local/bin/hamn")).unwrap();
    let legacy = bootstrap(&installer, &legacy_home, &tmp, system, &[], &local);
    assert_ne!(legacy.returncode, 0, "bootstrap replaced a regular executable without rollback evidence");
    assert!(legacy.stderr().contains("Move it aside and run this installer again"), "{}", legacy.stderr());
    assert_eq!(sha256_file(&legacy_home.join(".local/bin/hamn")).unwrap(), legacy_hash);
    assert!(!fs::symlink_metadata(legacy_home.join(".local/bin/hamn")).unwrap().file_type().is_symlink());
    assert!(!legacy_home.join(".hamn").exists());

    // A verified digest cache is independent of later origin damage.
    let mut bytes = fs::read(&host_artifact).unwrap();
    bytes.extend_from_slice(b"tampered\n");
    fs::write(&host_artifact, bytes).unwrap();
    let cached = bootstrap(&installer, &home, &tmp, system, &[], &local);
    assert_eq!(cached.returncode, 0, "{}", cached.stderr());
    assert!(cached.stderr().lines().any(|line| line == "Hamn 0.0.1 is already installed."), "{}", cached.stderr());
    assert_eq!(fs::read_link(&command).unwrap(), target);
    // Without those verified bytes, the corrupted origin is rejected
    // before any cutover.
    let cached_artifact = cache.join(format!("downloads/{host_hash}.artifact"));
    fs::remove_file(&cached_artifact).unwrap();
    let tampered = bootstrap(&installer, &home, &tmp, system, &[], &local);
    assert_ne!(tampered.returncode, 0, "bootstrap accepted a modified host artifact");
    let stderr = tampered.stderr();
    assert!(stderr.contains("exceeds size limit") || stderr.contains("artifact size or SHA-256 mismatch"), "{stderr}");
    assert_eq!(fs::read_link(&command).unwrap(), target);
    assert!(fs::symlink_metadata(&cached_artifact).is_err());
}

/// A Hamn 0.1.2 user runs install.sh once: the published installer of a
/// newer candidate migrates the genuine 0.1.2 install at the default paths
/// (made by the 0.1.2 installer, see `support::released`) in place, keeping
/// the 0.1.2 generation as the predecessor.
fn candidate_installer_migrates_a_released_install() {
    let work = TempDir::new("hamn-release-artifacts-");
    let root = fs::canonicalize(work.path()).unwrap();
    let guest = guest_fixture(&root);
    let candidate = root.join("candidate");
    let manifest_url = format!("file://{}", root.join("manifest.json").display());
    build_candidate(
        &guest,
        &candidate,
        &[
            ("RELEASE_TAG", OsStr::new("v0.2.0-rc.1")),
            ("HAMN_RELEASE_MANIFEST_URL", OsStr::new(&manifest_url)),
            ("HAMN_RELEASE_ALLOW_LOCAL", OsStr::new("1")),
        ],
    )
    .succeeded();
    let home = root.join("home");
    let tmp = root.join("tmp");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&tmp).unwrap();
    let (bindir, datadir) = (home.join(".local/bin"), home.join(".local/share/hamn/src"));
    let payload = released::payload(&root);
    let (old_target, _) = released::install(&payload, &bindir, &datadir, &home);
    let old_generation = upgrade::generation_of(&old_target);
    let old_binary = fs::read(&old_target).unwrap();

    let path =
        search_path(&[&bindir, Path::new("/usr/bin"), Path::new("/bin"), Path::new("/usr/sbin"), Path::new("/sbin")]);
    let migrated = bootstrap(
        &candidate.join("install.sh"),
        &home,
        &tmp,
        &path,
        &[],
        &[("SHELL", "/bin/zsh"), ("HAMN_INSTALL_ALLOW_LOCAL_ARTIFACTS", "1")],
    );
    assert_eq!(migrated.returncode, 0, "{}", migrated.stderr());
    let stderr = migrated.stderr();
    for line in [
        "Installing Hamn 0.2.0 for Apple Silicon macOS...",
        "Updating Hamn 0.1.2 → 0.2.0...",
        "Updated Hamn 0.1.2 → 0.2.0. Existing VMs were not restarted.",
        "Run hamn to get started. Update later with hamn upgrade.",
    ] {
        assert!(stderr.lines().any(|text| text == line), "missing {line:?}: {stderr}");
    }
    let command = bindir.join("hamn");
    let target = fs::read_link(&command).unwrap();
    let generation = upgrade::generation_of(&target);
    assert_ne!(generation, old_generation);
    let member = "hamn-v0.2.0-darwin-arm64";
    assert_eq!(
        fs::read(&target).unwrap(),
        archive_bytes(&candidate.join(format!("{member}.tar.gz")), &format!("{member}/bin/hamn"))
    );
    assert!(fs::read_to_string(generation.join(".hamn-generation")).unwrap().starts_with("version=2\n"));
    assert_eq!(
        fs::read_to_string(generation.join(".hamn-previous-target")).unwrap(),
        format!("{}\n", old_target.display())
    );
    assert_eq!(fs::read(&old_target).unwrap(), old_binary, "the 0.1.2 predecessor changed");
    assert!(old_generation.join("share/hamn/src/scripts").is_dir());
    let version = upgrade::run(Command::new(&command).arg("--version").env("HOME", &home), INSTALL);
    assert_eq!(version.stdout(), "hamn 0.2.0\n");
}

/// The bytes of archive member `member`.
fn archive_bytes(archive: &Path, member: &str) -> Vec<u8> {
    let output = Command::new("/usr/bin/tar").arg("-xOf").arg(archive).arg(member).output().unwrap();
    assert!(output.status.success(), "tar {member}: {}", String::from_utf8_lossy(&output.stderr));
    output.stdout
}
