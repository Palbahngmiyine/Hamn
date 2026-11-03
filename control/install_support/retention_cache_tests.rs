use super::*;
use crate::install_support::{locks, test_support::Temp};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::process::{Command, Stdio};

fn digest(n: u8) -> String {
    format!("{n:064x}")
}

fn write(path: &Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Sets the BSD file flags of `path` (for example `UF_IMMUTABLE`).
fn set_flags(path: &Path, flags: libc::c_uint) {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::chflags(c.as_ptr(), flags) }, 0, "{path:?}");
}

fn receipt_of(host: &str, guest: &str) -> String {
    format!(
        r#"{{"schemaVersion":2,"version":"v1.2.3","hostSHA256":"{host}","guestSHA256":"{guest}","installedSHA256":"{}"}}"#,
        digest(201)
    )
}

/// A receipt whose host archive is one that no test plants in `downloads`.
fn receipt(guest: &str) -> String {
    receipt_of(&digest(200), guest)
}

/// A HOME's image cache and the generations around it, in a canonical
/// temporary directory.
struct Home {
    temp: Temp,
    cache: PathBuf,
}

impl Drop for Home {
    /// Restores what a test made undeletable, even after a failed assertion.
    fn drop(&mut self) {
        fn restore(path: &Path) {
            let Ok(info) = fs::symlink_metadata(path) else {
                return;
            };
            if info.file_type().is_symlink() {
                return;
            }
            let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
            unsafe { libc::chflags(c.as_ptr(), 0) };
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
            if info.is_dir() {
                for child in fs::read_dir(path).into_iter().flatten().flatten() {
                    restore(&child.path());
                }
            }
        }
        restore(&self.temp.0);
    }
}

impl Home {
    fn new() -> Self {
        let temp = Temp::new();
        let cache = fs::canonicalize(&temp.0).unwrap().join("cache");
        fs::create_dir(&cache).unwrap();
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(cache.join("downloads")).unwrap();
        fs::set_permissions(cache.join("downloads"), fs::Permissions::from_mode(0o700)).unwrap();
        Self { temp, cache }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.cache.join(name)
    }

    fn artifact(&self, n: u8) -> PathBuf {
        let path = self.path(&format!("downloads/{}.artifact", digest(n)));
        write(&path, b"artifact", 0o600);
        path
    }

    fn image(&self, n: u8) -> PathBuf {
        let path = self.path(&format!("hamn-guest-{}.img", digest(n)));
        write(&path, b"image", 0o644);
        write(
            &self.path(&format!("hamn-guest-{}.img.verified", digest(n))),
            format!("{}\n", digest(n)).as_bytes(),
            0o644,
        );
        path
    }

    /// A raw base as `raw_cache.c` publishes it, with its digest lock.
    fn raw(&self, n: u8) -> PathBuf {
        let bundle = self.path(&format!("raw-v1-{}", digest(n)));
        fs::create_dir(&bundle).unwrap();
        write(&bundle.join("disk.raw"), b"raw", 0o400);
        write(&bundle.join("verified"), b"marker", 0o400);
        fs::set_permissions(&bundle, fs::Permissions::from_mode(0o700)).unwrap();
        write(
            &self.path(&format!(".raw-v1-{}.lock", digest(n))),
            b"",
            0o600,
        );
        bundle
    }

    fn select(&self, n: u8) {
        let hash = digest(n);
        write(
            &self.path("guest-image.json"),
            format!(r#"{{"schemaVersion":1,"file":"hamn-guest-{hash}.img","sha256":"{hash}"}}"#)
                .as_bytes(),
            0o644,
        );
    }

    /// A retained generation directory whose receipt names host archive
    /// `host` and guest image `guest`.
    fn release(&self, name: &str, host: u8, guest: u8) -> PathBuf {
        let path = self.temp.0.join("generations").join(name);
        fs::create_dir_all(&path).unwrap();
        write(
            &path.join(".hamn-release.json"),
            receipt_of(&digest(host), &digest(guest)).as_bytes(),
            0o600,
        );
        path
    }

    /// A retained generation directory whose receipt names guest `n`.
    fn generation(&self, name: &str, n: u8) -> PathBuf {
        self.release(name, 200, n)
    }

    /// Everything a populated cache holds for each digest in `digests`.
    fn populate(&self, digests: &[u8]) {
        for n in digests {
            self.artifact(*n);
            self.image(*n);
            self.raw(*n);
        }
    }

    fn entries(&self, n: u8) -> [PathBuf; 4] {
        let d = digest(n);
        [
            self.path(&format!("hamn-guest-{d}.img")),
            self.path(&format!("hamn-guest-{d}.img.verified")),
            self.path(&format!("raw-v1-{d}")),
            self.path(&format!("downloads/{d}.artifact")),
        ]
    }

    fn run(&self, retained: &[PathBuf]) -> Collection {
        self.run_with(retained, &[])
    }

    fn run_with(&self, retained: &[PathBuf], opened: &[PathBuf]) -> Collection {
        let opened: Vec<String> = opened.iter().map(|p| p.display().to_string()).collect();
        collect_cache_with(&self.cache, retained, || Ok(opened)).unwrap()
    }
}

#[test]
fn only_entries_outside_the_keep_set_are_removed() {
    // (selected, receipt guests of the retained generations, kept digests)
    let table: [(u8, &[u8], &[u8]); 5] = [
        (1, &[1], &[1]),
        (1, &[1, 2], &[1, 2]),
        (2, &[1], &[1, 2]),
        (3, &[3, 3], &[3]),
        (4, &[1, 2, 3], &[1, 2, 3, 4]),
    ];
    for (selected, guests, expected) in table {
        let home = Home::new();
        home.populate(&[1, 2, 3, 4, 5]);
        home.select(selected);
        let retained: Vec<PathBuf> = guests
            .iter()
            .enumerate()
            .map(|(i, n)| home.generation(&format!("g{i}"), *n))
            .collect();
        let report = home.run(&retained);
        for n in 1..=5 {
            let [image, marker, raw, artifact] = home.entries(n);
            let keeps = expected.contains(&n);
            for entry in [&image, &marker, &raw] {
                assert_eq!(exists(entry), keeps, "{selected} {guests:?}: {entry:?}");
            }
            assert!(!exists(&artifact), "{selected} {guests:?}: {artifact:?}");
            assert!(
                exists(&home.path(&format!(".raw-v1-{}.lock", digest(n)))),
                "the digest lock stays"
            );
        }
        assert_eq!(
            report.removed.len(),
            5 + (5 - expected.len()) * 3,
            "{selected} {guests:?}: {report:?}"
        );
        assert!(report.deferred.is_empty(), "{report:?}");
        assert!(exists(&home.path("guest-image.json")));
        let again = home.run(&retained);
        assert!(
            again.removed.is_empty() && again.deferred.is_empty(),
            "{again:?}"
        );
    }
}

#[test]
fn an_unusable_receipt_of_a_retained_generation_defers_images_raw_bases_and_unknown_downloads() {
    let valid = receipt(&digest(1));
    let variants: [(&str, Option<String>); 8] = [
        ("missing", None),
        ("malformed", Some("{".into())),
        (
            "other schema",
            Some(valid.replace("\"schemaVersion\":2", "\"schemaVersion\":1")),
        ),
        (
            "uppercase digest",
            Some(valid.replace(&digest(1), &"A".repeat(64))),
        ),
        ("short digest", Some(valid.replace(&digest(1), "abc"))),
        (
            "uppercase host digest",
            Some(valid.replace(&digest(200), &"A".repeat(64))),
        ),
        (
            "short host digest",
            Some(valid.replace(&digest(200), "abc")),
        ),
        ("unknown field", Some(valid.replace('}', ",\"extra\":1}"))),
    ];
    for (label, text) in variants {
        let home = Home::new();
        home.populate(&[1, 2]);
        home.select(1);
        let good = home.generation("good", 1);
        let bad = home.generation("bad", 2);
        match text {
            None => fs::remove_file(bad.join(".hamn-release.json")).unwrap(),
            Some(text) => write(&bad.join(".hamn-release.json"), text.as_bytes(), 0o600),
        }
        let report = home.run(&[good, bad.clone()]);
        for n in [1, 2] {
            let [image, marker, raw, _] = home.entries(n);
            assert!(
                exists(&image) && exists(&marker) && exists(&raw),
                "{label}: {n}"
            );
        }
        assert!(
            !exists(&home.entries(1)[3]),
            "{label}: the selected image's download is no host archive"
        );
        assert!(
            exists(&home.entries(2)[3]),
            "{label}: the unreadable receipt may name this download as its host archive"
        );
        assert_eq!(report.deferred.len(), 2, "{label}: {report:?}");
        for message in &report.deferred {
            assert!(
                message.contains(bad.to_str().unwrap()),
                "{label}: {report:?}"
            );
        }
        let downloads = report.deferred.iter().filter(|m| m.contains("downloads"));
        assert_eq!(downloads.count(), 1, "{label}: {report:?}");
    }
}

#[test]
fn the_host_archive_of_a_retained_generation_stays() {
    let home = Home::new();
    home.select(1);
    // 10 and 11: host archives of the retained generations; 12: one of an
    // earlier release; 1, 2 and 3: guest images' downloads.
    for n in [10, 11, 12, 1, 2, 3] {
        home.artifact(n);
    }
    let retained = [
        home.release("active", 10, 1),
        home.release("previous", 11, 2),
    ];
    let report = home.run(&retained);
    for kept in [10, 11] {
        assert!(exists(&home.entries(kept)[3]), "{kept}: {report:?}");
    }
    for gone in [12, 1, 2, 3] {
        assert!(!exists(&home.entries(gone)[3]), "{gone}: {report:?}");
    }
    assert_eq!(report.removed.len(), 4, "{report:?}");
    assert!(report.deferred.is_empty(), "{report:?}");
    let again = collect_cache_with(&home.cache, &retained, || {
        panic!("only retained host archives are left")
    })
    .unwrap();
    assert!(
        again.removed.is_empty() && again.deferred.is_empty(),
        "{again:?}"
    );
}

#[test]
fn an_unusable_receipt_leaves_only_downloads_that_could_be_its_host_archive() {
    let home = Home::new();
    home.select(1);
    // 10: host archive named by the readable receipt; 1: selected image;
    // 2: that receipt's guest image; 5: of unknown kind.
    for n in [10, 1, 2, 5] {
        home.artifact(n);
    }
    let good = home.release("good", 10, 2);
    let bad = home.generation("bad", 3);
    fs::remove_file(bad.join(".hamn-release.json")).unwrap();
    let report = home.run(&[good, bad.clone()]);
    assert!(exists(&home.entries(10)[3]) && exists(&home.entries(5)[3]));
    assert!(!exists(&home.entries(1)[3]) && !exists(&home.entries(2)[3]));
    assert_eq!(report.removed.len(), 2, "{report:?}");
    assert_eq!(report.deferred.len(), 1, "{report:?}");
    assert!(
        report.deferred[0].contains("downloads")
            && report.deferred[0].contains(bad.to_str().unwrap()),
        "{report:?}"
    );
}

#[test]
fn an_unsafe_receipt_file_is_not_trusted() {
    for mode in [0o644, 0o666] {
        let home = Home::new();
        home.populate(&[1, 2]);
        home.select(1);
        let generation = home.generation("g", 1);
        fs::set_permissions(
            generation.join(".hamn-release.json"),
            fs::Permissions::from_mode(mode),
        )
        .unwrap();
        let report = home.run(&[generation]);
        assert!(exists(&home.entries(2)[0]), "{mode:o}");
        assert!(exists(&home.entries(2)[3]), "{mode:o}");
        assert_eq!(report.deferred.len(), 2, "{mode:o}: {report:?}");
    }
    let home = Home::new();
    home.populate(&[1, 2]);
    home.select(1);
    let generation = home.generation("g", 1);
    let elsewhere = home.temp.0.join("elsewhere.json");
    fs::rename(generation.join(".hamn-release.json"), &elsewhere).unwrap();
    symlink(&elsewhere, generation.join(".hamn-release.json")).unwrap();
    let report = home.run(&[generation]);
    assert!(
        exists(&home.entries(2)[0]) && exists(&home.entries(2)[3]),
        "a symlinked receipt named no image"
    );
    assert_eq!(report.deferred.len(), 2, "{report:?}");
}

#[test]
fn pending_recovery_metadata_deletes_nothing() {
    for pending in [".hamn-update-transaction", ".hamn-update-retired.abc123"] {
        let home = Home::new();
        home.populate(&[1, 2]);
        home.select(1);
        fs::create_dir(home.path(pending)).unwrap();
        let generation = home.generation("g", 1);
        let report = home.run(&[generation]);
        assert!(report.removed.is_empty(), "{pending}: {report:?}");
        assert_eq!(report.deferred.len(), 1, "{pending}: {report:?}");
        for n in [1, 2] {
            assert!(home.entries(n).iter().all(|p| exists(p)), "{pending}: {n}");
        }
    }
    let home = Home::new();
    home.populate(&[2]);
    home.select(1);
    fs::set_permissions(&home.cache, fs::Permissions::from_mode(0o775)).unwrap();
    let report = home.run(&[home.generation("g", 1)]);
    assert!(
        report.removed.is_empty() && report.deferred.len() == 1,
        "{report:?}"
    );
    assert!(home.entries(2).iter().all(|p| exists(p)));
}

#[test]
fn an_unreadable_selection_defers_images_and_raw_bases_only() {
    for damage in ["missing", "malformed", "another file"] {
        let home = Home::new();
        home.populate(&[1, 2]);
        let generation = home.generation("g", 1);
        match damage {
            "malformed" => write(&home.path("guest-image.json"), b"{", 0o644),
            "another file" => write(
                &home.path("guest-image.json"),
                format!(
                    r#"{{"schemaVersion":1,"file":"hamn-guest-{}.img","sha256":"{}"}}"#,
                    digest(2),
                    digest(1)
                )
                .as_bytes(),
                0o644,
            ),
            _ => (),
        }
        let report = home.run(&[generation]);
        for n in [1, 2] {
            let [image, marker, raw, artifact] = home.entries(n);
            assert!(
                exists(&image) && exists(&marker) && exists(&raw),
                "{damage}: {n}"
            );
            assert!(!exists(&artifact), "{damage}");
        }
        assert_eq!(report.deferred.len(), 1, "{damage}: {report:?}");
    }
}

#[test]
fn entries_open_in_a_process_stay_and_are_reported() {
    let home = Home::new();
    home.populate(&[1, 2, 3, 4]);
    home.select(1);
    let generation = home.generation("g", 1);
    let opened = [
        home.entries(2)[0].clone(),
        home.entries(3)[3].clone(),
        home.entries(4)[2].join("disk.raw"),
    ];
    let report = home.run_with(&[generation], &opened);
    assert!(exists(&home.entries(2)[0]), "an open image stays");
    assert!(
        !exists(&home.entries(2)[1]),
        "its marker is a different file"
    );
    assert!(exists(&home.entries(3)[3]), "an open artifact stays");
    assert!(
        exists(&home.entries(4)[2]),
        "a bundle with an open file stays"
    );
    assert!(!exists(&home.entries(3)[0]) && !exists(&home.entries(2)[3]));
    assert_eq!(report.deferred.len(), 3, "{report:?}");
}

#[test]
fn a_failed_open_file_scan_deletes_nothing() {
    let home = Home::new();
    home.populate(&[1, 2]);
    home.select(1);
    let generation = home.generation("g", 1);
    let report = collect_cache_with(&home.cache, &[generation], || {
        Err("open-file scan unavailable or incomplete".into())
    })
    .unwrap();
    assert!(report.removed.is_empty(), "{report:?}");
    assert_eq!(report.deferred.len(), 1, "{report:?}");
    for n in [1, 2] {
        assert!(home.entries(n).iter().all(|p| exists(p)));
    }
}

#[test]
fn symbolic_links_stay_and_their_targets_are_never_followed() {
    let home = Home::new();
    home.select(1);
    let victim = home.temp.0.join("victim");
    fs::create_dir(&victim).unwrap();
    write(&victim.join("disk.raw"), b"victim", 0o400);
    write(&victim.join("verified"), b"victim", 0o400);
    write(&victim.join("file"), b"victim", 0o644);
    let [image, marker, bundle, artifact] = home.entries(2);
    symlink(victim.join("file"), &image).unwrap();
    symlink(victim.join("file"), &marker).unwrap();
    symlink(&victim, &bundle).unwrap();
    write(
        &home.path(&format!(".raw-v1-{}.lock", digest(2))),
        b"",
        0o600,
    );
    symlink(victim.join("file"), &artifact).unwrap();
    let report = home.run(&[home.generation("g", 1)]);
    for entry in [&image, &marker, &bundle, &artifact] {
        assert!(
            fs::symlink_metadata(entry)
                .unwrap()
                .file_type()
                .is_symlink(),
            "{entry:?}"
        );
    }
    assert_eq!(
        fs::read_dir(&victim).unwrap().count(),
        3,
        "a link target was changed"
    );
    assert_eq!(report.deferred.len(), 4, "{report:?}");

    let home = Home::new();
    home.select(1);
    let other = home.temp.0.join("other-downloads");
    fs::create_dir(&other).unwrap();
    write(
        &other.join(format!("{}.artifact", digest(2))),
        b"artifact",
        0o600,
    );
    fs::remove_dir(home.path("downloads")).unwrap();
    symlink(&other, home.path("downloads")).unwrap();
    let report = home.run(&[home.generation("g", 1)]);
    assert!(
        exists(&other.join(format!("{}.artifact", digest(2)))),
        "{report:?}"
    );
    assert_eq!(report.deferred.len(), 1, "{report:?}");
}

#[test]
fn entries_with_other_links_stay() {
    let home = Home::new();
    home.populate(&[1, 2]);
    home.select(1);
    let [image, marker, bundle, artifact] = home.entries(2);
    let aliases = home.temp.0.join("aliases");
    fs::create_dir(&aliases).unwrap();
    for (i, entry) in [&image, &marker, &artifact, &bundle.join("disk.raw")]
        .iter()
        .enumerate()
    {
        fs::hard_link(entry, aliases.join(i.to_string())).unwrap();
    }
    let report = home.run(&[home.generation("g", 1)]);
    for entry in [&image, &marker, &bundle, &artifact] {
        assert!(exists(entry), "{entry:?}");
    }
    assert_eq!(report.deferred.len(), 4, "{report:?}");
    assert_eq!(fs::read(aliases.join("0")).unwrap(), b"image");
}

#[test]
fn a_file_owned_by_another_user_stays() {
    // A real file owned by root: removing it would also fail, so only the
    // ownership rule keeps it from being selected.
    let foreign = Path::new("/private/etc/hosts");
    let info = fs::symlink_metadata(foreign).unwrap();
    assert!(info.is_file() && info.nlink() == 1 && info.uid() != files::uid());
    assert!(stays(foreign, false, &[]).unwrap().is_some());
    assert!(
        stays(Path::new("/private/etc"), true, &[])
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_raw_base_whose_digest_lock_is_held_stays_until_it_is_released() {
    let home = Home::new();
    home.populate(&[1, 2]);
    home.select(1);
    let generation = home.generation("g", 1);
    let lock = download::lock(&home.path(&format!(".raw-v1-{}.lock", digest(2))), true).unwrap();
    let report = home.run(std::slice::from_ref(&generation));
    assert!(
        exists(&home.entries(2)[2]),
        "the locked raw base was removed"
    );
    assert!(
        !exists(&home.entries(2)[0]),
        "the lock covers only the raw base"
    );
    assert_eq!(report.deferred.len(), 1, "{report:?}");
    drop(lock);
    let report = home.run(&[generation]);
    assert!(
        !exists(&home.entries(2)[2]) && report.deferred.is_empty(),
        "{report:?}"
    );
}

#[test]
fn a_raw_base_with_unexpected_content_stays() {
    for extra in ["note", "nested/"] {
        let home = Home::new();
        home.populate(&[2]);
        home.select(1);
        let bundle = home.entries(2)[2].clone();
        if let Some(directory) = extra.strip_suffix('/') {
            fs::create_dir(bundle.join(directory)).unwrap();
        } else {
            write(&bundle.join(extra), b"user data", 0o600);
        }
        let report = home.run(&[home.generation("g", 1)]);
        assert!(exists(&bundle.join("disk.raw")), "{extra}: {report:?}");
        assert_eq!(report.deferred.len(), 1, "{extra}: {report:?}");
    }
}

#[test]
fn an_interrupted_raw_erase_leaves_no_half_bundle_and_the_next_run_finishes() {
    let home = Home::new();
    home.populate(&[2]);
    home.select(1);
    let generation = home.generation("g", 1);
    let live = home.entries(2)[2].clone();
    let retired = home.path(&format!("raw-v1-{}.retired", digest(2)));
    // A file that cannot be unlinked fails the erase after the bundle left
    // its live name and its other file is gone.
    set_flags(&live.join("disk.raw"), libc::UF_IMMUTABLE);
    let report = home.run(std::slice::from_ref(&generation));
    assert!(!exists(&live), "a half-erased bundle kept its live name");
    assert!(exists(&retired.join("disk.raw")) && !exists(&retired.join("verified")));
    assert_eq!(report.deferred.len(), 1, "{report:?}");
    // Another failure while recovering from the first one.
    let again = home.run(std::slice::from_ref(&generation));
    assert!(
        exists(&retired.join("disk.raw")) && again.deferred.len() == 1,
        "{again:?}"
    );
    set_flags(&retired.join("disk.raw"), 0);
    let finished = home.run(&[generation]);
    assert!(
        !exists(&retired) && finished.deferred.is_empty(),
        "{finished:?}"
    );
    assert!(exists(&home.path(&format!(".raw-v1-{}.lock", digest(2)))));
}

#[test]
fn a_leftover_retired_raw_base_is_erased_even_for_a_kept_digest() {
    let home = Home::new();
    home.populate(&[1]);
    home.select(1);
    let retired = home.path(&format!("raw-v1-{}.retired", digest(1)));
    fs::create_dir(&retired).unwrap();
    write(&retired.join("disk.raw"), b"raw", 0o400);
    fs::set_permissions(&retired, fs::Permissions::from_mode(0o700)).unwrap();
    let report = home.run(&[home.generation("g", 1)]);
    assert!(!exists(&retired), "{report:?}");
    assert!(
        home.entries(1).iter().take(3).all(|p| exists(p)),
        "the kept base stays"
    );
}

#[test]
fn a_failed_removal_defers_only_that_entry() {
    let home = Home::new();
    home.populate(&[2, 3]);
    home.select(1);
    fs::set_permissions(home.path("downloads"), fs::Permissions::from_mode(0o500)).unwrap();
    let report = home.run(&[home.generation("g", 1)]);
    assert_eq!(report.deferred.len(), 2, "{report:?}");
    assert!(exists(&home.entries(2)[3]) && exists(&home.entries(3)[3]));
    assert!(
        !exists(&home.entries(2)[0]) && !exists(&home.entries(3)[2]),
        "{report:?}"
    );
    fs::set_permissions(home.path("downloads"), fs::Permissions::from_mode(0o700)).unwrap();
    let again = home.run(&[home.generation("g", 1)]);
    assert!(
        !exists(&home.entries(2)[3]) && again.deferred.is_empty(),
        "{again:?}"
    );
}

#[test]
fn a_collection_without_retained_generations_is_refused() {
    let home = Home::new();
    home.populate(&[2]);
    home.select(1);
    assert!(collect_cache_with(&home.cache, &[], || Ok(Vec::new())).is_err());
    assert!(home.entries(2).iter().all(|p| exists(p)));
}

#[test]
fn a_cache_that_only_holds_the_selection_is_left_alone_without_scanning() {
    let home = Home::new();
    fs::remove_dir(home.path("downloads")).unwrap();
    home.image(1);
    home.raw(1);
    home.select(1);
    // The retained generation has no receipt, as after `make install`.
    let source_build = home.temp.0.join("generations/source-build");
    fs::create_dir_all(&source_build).unwrap();
    let report = collect_cache_with(&home.cache, &[source_build], || {
        panic!("nothing needed the open-file scan")
    })
    .unwrap();
    assert!(
        report.removed.is_empty() && report.deferred.is_empty(),
        "{report:?}"
    );
}

/// A child process that holds `path` open (descriptor 3) until it is killed.
struct Holder(std::process::Child);

impl Holder {
    fn open(path: &Path) -> Self {
        use std::io::BufRead;
        let mut child = Command::new("/bin/sh")
            .args(["-c", r#"exec 3<"$1" && echo ready && exec /bin/sleep 120"#])
            .arg("sh")
            .arg(path)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line, "ready\n");
        Self(child)
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_real_open_file_scan_sees_a_process_holding_an_image() {
    let home = Home::new();
    home.populate(&[2]);
    home.select(1);
    let generation = home.generation("g", 1);
    let roots = locks::Roots::prepare(
        &home.temp.0.join("bin"),
        &home.temp.0.join("share/hamn/src"),
    )
    .unwrap();
    let transaction = locks::Transaction::acquire(&roots).unwrap();
    let updater = locks::cache(&home.path(".hamn-upgrade.lock"), &transaction).unwrap();
    // A cache below a symbolic link, as under a HOME below /var.
    let alias = home.temp.0.join("home-alias");
    symlink(fs::canonicalize(&home.temp.0).unwrap(), &alias).unwrap();
    let link = alias.join("cache");
    let holder = Holder::open(&home.entries(2)[0]);
    let report = collect_cache(&link, std::slice::from_ref(&generation), &updater).unwrap();
    assert!(exists(&home.entries(2)[0]), "{report:?}");
    assert!(
        !exists(&home.entries(2)[2]),
        "unopened entries still go: {report:?}"
    );
    drop(holder);
    let report = collect_cache(&link, &[generation], &updater).unwrap();
    assert!(!exists(&home.entries(2)[0]), "{report:?}");
}

#[test]
fn retained_generations_are_the_kept_targets_inside_the_generation_root() {
    let root = Path::new("/data/.hamn-generations");
    let one = format!("{}-abcdef", "1".repeat(64));
    let two = format!("{}-abcdef", "2".repeat(64));
    let target = |name: &str| root.join(name).join("bin/hamn");
    let keep = vec![
        target(&one),
        target(&two),
        target(&one),
        PathBuf::from("/elsewhere/.hamn-generations")
            .join(&one)
            .join("bin/hamn"),
        root.join("not-a-generation/bin/hamn"),
    ];
    assert_eq!(
        retained_generations(&keep, root).unwrap(),
        [root.join(&one), root.join(&two)]
    );
}

#[test]
fn a_symbolic_link_inside_a_raw_base_keeps_the_base_and_its_target() {
    let home = Home::new();
    home.populate(&[2]);
    home.select(1);
    let bundle = home.entries(2)[2].clone();
    let victim = home.temp.0.join("victim");
    write(&victim, b"victim", 0o600);
    fs::remove_file(bundle.join("disk.raw")).unwrap();
    symlink(&victim, bundle.join("disk.raw")).unwrap();
    let report = home.run(&[home.generation("g", 1)]);
    assert!(
        exists(&bundle.join("verified")) && exists(&victim),
        "{report:?}"
    );
    assert_eq!(report.deferred.len(), 1, "{report:?}");
}

#[test]
fn a_retired_and_a_live_raw_base_of_an_unused_digest_both_go() {
    let home = Home::new();
    home.populate(&[2]);
    home.select(1);
    let retired = home.path(&format!("raw-v1-{}.retired", digest(2)));
    fs::create_dir(&retired).unwrap();
    write(&retired.join("disk.raw"), b"raw", 0o400);
    fs::set_permissions(&retired, fs::Permissions::from_mode(0o700)).unwrap();
    let report = home.run(&[home.generation("g", 1)]);
    assert!(
        !exists(&retired) && !exists(&home.entries(2)[2]),
        "{report:?}"
    );
    assert!(report.deferred.is_empty(), "{report:?}");
}

/// Fixture for the leftover `raw-v1-<d>.retired` of digest `n`, with the
/// digest lock `remove_raw` takes.
fn retired_path(home: &Home, n: u8) -> PathBuf {
    write(
        &home.path(&format!(".raw-v1-{}.lock", digest(n))),
        b"",
        0o600,
    );
    home.path(&format!("raw-v1-{}.retired", digest(n)))
}

fn make_retired_bundle(retired: &Path) {
    fs::create_dir(retired).unwrap();
    write(&retired.join("disk.raw"), b"raw", 0o400);
    write(&retired.join("verified"), b"marker", 0o400);
    fs::set_permissions(retired, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn a_leftover_retired_raw_base_that_is_a_symbolic_link_stays_and_its_target_is_untouched() {
    let home = Home::new();
    home.select(1);
    let victim = home.temp.0.join("victim");
    fs::create_dir(&victim).unwrap();
    write(&victim.join("disk.raw"), b"victim", 0o400);
    write(&victim.join("verified"), b"victim", 0o400);
    let retired = retired_path(&home, 2);
    symlink(&victim, &retired).unwrap();
    let report = home.run(&[home.generation("g", 1)]);
    assert!(
        fs::symlink_metadata(&retired)
            .unwrap()
            .file_type()
            .is_symlink(),
        "{report:?}"
    );
    assert!(
        exists(&victim.join("disk.raw")) && exists(&victim.join("verified")),
        "a link target was changed: {report:?}"
    );
    assert_eq!(report.deferred.len(), 1, "{report:?}");
}

#[test]
fn a_leftover_retired_raw_base_with_unexpected_content_stays_whole() {
    for extra in ["note", "nested/"] {
        let home = Home::new();
        home.select(1);
        let retired = retired_path(&home, 2);
        make_retired_bundle(&retired);
        if let Some(directory) = extra.strip_suffix('/') {
            fs::create_dir(retired.join(directory)).unwrap();
        } else {
            write(&retired.join(extra), b"user data", 0o600);
        }
        let report = home.run(&[home.generation("g", 1)]);
        assert!(
            exists(&retired.join("disk.raw")) && exists(&retired.join("verified")),
            "{extra}: {report:?}"
        );
        assert_eq!(report.deferred.len(), 1, "{extra}: {report:?}");
    }
}

#[test]
fn a_leftover_retired_raw_base_with_an_open_file_stays_whole_until_it_is_closed() {
    let home = Home::new();
    home.select(1);
    let generation = home.generation("g", 1);
    let retired = retired_path(&home, 2);
    make_retired_bundle(&retired);
    let scan = || open_files(Duration::from_secs(30));
    let holder = Holder::open(&retired.join("disk.raw"));
    let report = collect_cache_with(&home.cache, std::slice::from_ref(&generation), scan).unwrap();
    assert!(
        exists(&retired.join("disk.raw")) && exists(&retired.join("verified")),
        "{report:?}"
    );
    assert_eq!(report.deferred.len(), 1, "{report:?}");
    drop(holder);
    let report = collect_cache_with(&home.cache, &[generation], scan).unwrap();
    assert!(!exists(&retired), "{report:?}");
}

#[test]
fn an_image_and_its_marker_are_removed_independently() {
    let home = Home::new();
    home.select(1);
    home.image(2);
    home.image(3);
    let [image, marker, ..] = home.entries(2);
    fs::remove_file(&image).unwrap();
    fs::remove_file(&home.entries(3)[1]).unwrap();
    let report = home.run(&[home.generation("g", 1)]);
    assert!(
        !exists(&marker) && !exists(&home.entries(3)[0]),
        "{report:?}"
    );
    assert!(report.deferred.is_empty(), "{report:?}");
}

#[test]
fn an_artifact_whose_download_lock_is_held_stays_until_it_is_released() {
    let home = Home::new();
    home.populate(&[2]);
    home.select(1);
    let generation = home.generation("g", 1);
    let lock = download::artifact_lock(&home.path("downloads"), &digest(2), true).unwrap();
    let report = home.run(std::slice::from_ref(&generation));
    assert!(exists(&home.entries(2)[3]), "{report:?}");
    assert!(
        !exists(&home.entries(2)[0]),
        "the lock covers only the download"
    );
    assert_eq!(report.deferred.len(), 1, "{report:?}");
    drop(lock);
    let report = home.run(&[generation]);
    assert!(
        !exists(&home.entries(2)[3]) && report.deferred.is_empty(),
        "{report:?}"
    );
}

#[test]
fn a_leftover_retired_raw_base_is_erased_while_the_keep_set_is_unknown() {
    let home = Home::new();
    home.select(1);
    home.image(2);
    let retired = home.path(&format!("raw-v1-{}.retired", digest(3)));
    fs::create_dir(&retired).unwrap();
    write(&retired.join("disk.raw"), b"raw", 0o400);
    fs::set_permissions(&retired, fs::Permissions::from_mode(0o700)).unwrap();
    // No receipt, as after `make install`: images stay, the retired base goes.
    let source_build = home.temp.0.join("generations/source-build");
    fs::create_dir_all(&source_build).unwrap();
    let report = home.run(&[source_build]);
    assert!(!exists(&retired), "{report:?}");
    assert!(exists(&home.entries(2)[0]), "{report:?}");
    assert_eq!(report.deferred.len(), 1, "{report:?}");
}

#[test]
fn names_that_are_not_cache_entries_are_never_touched() {
    let home = Home::new();
    home.select(1);
    let d = digest(2);
    let stage = home.path(&format!("raw-v1-{d}.stage"));
    fs::create_dir(&stage).unwrap();
    write(&stage.join("disk.raw"), b"stage", 0o600);
    let kept = [
        format!("hamn-guest-{}.img", "A".repeat(64)),
        format!("hamn-guest-{}.img", "ab"),
        format!("hamn-guest-{d}.img.extra"),
        format!(".hamn-guest-{d}.img.update.123"),
        format!(".hamn-guest-{d}.img.verified.abc123"),
        format!("raw-v1-{}", "ab"),
        format!("raw-v1-{d}.other"),
        format!("raw-v2-{d}"),
        format!("downloads/.{d}.partial"),
        format!("downloads/.{d}.validator"),
        format!("downloads/.{d}.lock"),
        format!("downloads/{d}.artifact.bak"),
        format!("downloads/{}.artifact", "ab"),
    ];
    for name in &kept {
        write(&home.path(name), b"unrelated", 0o600);
    }
    let report = collect_cache_with(&home.cache, &[home.generation("g", 1)], || {
        panic!("nothing is a cache entry")
    })
    .unwrap();
    assert!(
        report.removed.is_empty() && report.deferred.is_empty(),
        "{report:?}"
    );
    for name in &kept {
        assert!(exists(&home.path(name)), "{name}");
    }
    assert!(exists(&stage.join("disk.raw")));
}
