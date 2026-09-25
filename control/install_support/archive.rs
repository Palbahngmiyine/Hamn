//! Validate the complete gzip/tar before writing any destination entry. Only
//! canonical relative regular files/directories under one root are accepted.
//! PAX/GNU extended paths are interpreted by tar; links/devices are rejected.
use super::{Result, require};
use flate2::read::GzDecoder;
use std::io::{Seek, SeekFrom};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use tar::Archive;

fn name(entry: &tar::Entry<'_, GzDecoder<File>>) -> Result<String> {
    let bytes = entry.path_bytes();
    let raw = std::str::from_utf8(&bytes)?;
    require(
        !raw.ends_with('/') || entry.header().entry_type().is_dir(),
        "regular artifact path ends in a separator",
    )?;
    let path = raw.trim_end_matches('/');
    require(
        !path.is_empty()
            && !path.contains('\\')
            && path
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != ".."),
        "unsafe host artifact path",
    )?;
    require(
        entry.header().entry_type().is_file() || entry.header().entry_type().is_dir(),
        "host artifact contains a non-regular entry",
    )?;
    Ok(path.to_owned())
}
fn open(path: &Path) -> Result<Archive<GzDecoder<File>>> {
    Ok(Archive::new(GzDecoder::new(File::open(path)?)))
}

pub(super) fn extract(source: &Path, destination: &Path) -> Result<String> {
    let mut bundle = open(source)?;
    let mut roots = BTreeSet::new();
    let mut entries = BTreeMap::new();
    for item in bundle.entries()? {
        let mut entry = item?;
        let path = name(&entry)?;
        roots.insert(path.split('/').next().unwrap().to_owned());
        require(
            entries
                .insert(path, entry.header().entry_type().is_dir())
                .is_none(),
            "duplicate host artifact entry",
        )?;
        // Consume every payload now: truncated data is rejected before extraction.
        std::io::copy(&mut entry, &mut std::io::sink())?;
    }
    // Consume the gzip trailer too, so a truncated/checksum-invalid stream fails.
    let mut decoder = bundle.into_inner();
    std::io::copy(&mut decoder, &mut std::io::sink())?;
    require(
        roots.len() == 1,
        "host artifact must have one top-level directory",
    )?;
    let root = roots.into_iter().next().unwrap();
    // A release archive is one generation payload: the executable and its
    // manifest pointer. Other members are validated but never installed.
    for required in ["bin/hamn", super::generation::POINTER] {
        require(
            entries.get(&format!("{root}/{required}")) == Some(&false),
            "host artifact is missing required Hamn files",
        )?;
    }
    for path in entries.keys() {
        let mut parent = Path::new(path).parent();
        while let Some(p) = parent {
            require(
                entries.get(p.to_str().ok_or("invalid archive path")?) != Some(&false),
                "artifact file is used as a directory",
            )?;
            parent = p.parent();
        }
    }
    // Never merge into a caller-owned tree or follow an existing destination.
    fs::create_dir(destination)?;
    fs::set_permissions(destination, fs::Permissions::from_mode(0o700))?;
    // Reuse the validated file descriptor; a replacement at the pathname must
    // not select different archive bytes between validation and extraction.
    let mut file = decoder.into_inner();
    file.seek(SeekFrom::Start(0))?;
    let mut bundle = Archive::new(GzDecoder::new(file));
    let mut directories: Vec<(PathBuf, u32)> = Vec::new();
    for item in bundle.entries()? {
        let mut entry = item?;
        let path = destination.join(name(&entry)?);
        let mode = entry.header().mode()? & 0o777;
        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&path)?;
            directories.push((path, mode));
        } else {
            fs::create_dir_all(path.parent().ok_or("invalid archive parent")?)?;
            let mut out = File::options().write(true).create_new(true).open(&path)?;
            std::io::copy(&mut entry, &mut out)?;
            out.set_permissions(fs::Permissions::from_mode(mode))?;
        }
    }
    // Apply directory modes after children have been written.
    directories.sort_by_key(|(p, _)| std::cmp::Reverse(p.components().count()));
    for (path, mode) in directories {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_support::test_support::Temp;
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;
    use tar::{Builder, EntryType, Header};

    fn entry(builder: &mut Builder<GzEncoder<File>>, path: &str, kind: EntryType) {
        let mut header = Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o755);
        header.set_entry_type(kind);
        header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
        if kind.is_symlink() || kind.is_hard_link() {
            header.set_link_name("../../escape").unwrap();
        }
        header.set_cksum();
        builder.append(&header, &b"test"[..]).unwrap();
    }
    fn bundle(path: &Path, extra: Option<(&str, EntryType)>, pax: bool) {
        let mut b = Builder::new(GzEncoder::new(
            File::create(path).unwrap(),
            Compression::default(),
        ));
        for p in ["bin/hamn", "share/hamn/update-manifest-url"] {
            entry(&mut b, &format!("release/{p}"), EntryType::Regular);
        }
        if let Some((path, kind)) = extra {
            if pax {
                b.append_pax_extensions([("path", path.as_bytes())])
                    .unwrap();
                entry(&mut b, "release/safe", kind);
            } else {
                entry(&mut b, path, kind);
            }
        }
        b.into_inner().unwrap().finish().unwrap();
    }
    #[test]
    fn validates_complete_archive_before_creating_destination() {
        let t = Temp::new();
        let archive = t.0.join("host.tar.gz");
        let dest = t.0.join("extract");
        for (path, kind, pax) in [
            ("../escape", EntryType::Regular, false),
            ("/escape", EntryType::Regular, false),
            ("release/../escape", EntryType::Regular, false),
            ("release//bad", EntryType::Regular, false),
            ("release/file/", EntryType::Regular, false),
            ("release/link", EntryType::Symlink, false),
            ("release/link", EntryType::Link, false),
            ("release/device", EntryType::Char, false),
            ("other/file", EntryType::Regular, false),
            ("release/bin/hamn", EntryType::Regular, false),
            ("../escape", EntryType::Regular, true),
        ] {
            bundle(&archive, Some((path, kind)), pax);
            assert!(extract(&archive, &dest).is_err(), "accepted {path:?}");
            assert!(!dest.exists());
        }
        bundle(&archive, None, false);
        let mut bytes = fs::read(&archive).unwrap();
        bytes.truncate(bytes.len() - 8);
        File::create(&archive).unwrap().write_all(&bytes).unwrap();
        assert!(extract(&archive, &dest).is_err());
        assert!(!dest.exists());
    }
    #[test]
    fn extracts_exact_bytes_modes_and_pax_paths_without_overwriting_a_tree() {
        let t = Temp::new();
        let archive = t.0.join("host.tar.gz");
        let dest = t.0.join("extract");
        bundle(
            &archive,
            Some(("release/긴 파일", EntryType::Regular)),
            true,
        );
        assert_eq!(extract(&archive, &dest).unwrap(), "release");
        assert_eq!(fs::read(dest.join("release/긴 파일")).unwrap(), b"test");
        assert_eq!(
            fs::metadata(dest.join("release/bin/hamn"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert!(extract(&archive, &dest).is_err());
    }
    #[test]
    fn earlier_layout_archive_without_the_generation_pointer_is_refused() {
        // Earlier releases kept the pointer under packaging/release/ beside
        // shell scripts; such an archive is not a generation payload.
        let t = Temp::new();
        let archive = t.0.join("host.tar.gz");
        let mut b = Builder::new(GzEncoder::new(
            File::create(&archive).unwrap(),
            Compression::default(),
        ));
        for p in ["bin/hamn", "packaging/release/update-manifest-url"] {
            entry(&mut b, &format!("release/{p}"), EntryType::Regular);
        }
        b.into_inner().unwrap().finish().unwrap();
        let error = extract(&archive, &t.0.join("extract")).unwrap_err();
        assert_eq!(
            error.to_string(),
            "host artifact is missing required Hamn files"
        );
        assert!(!t.0.join("extract").exists());
    }
}
