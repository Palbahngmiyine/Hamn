//! `hamn qcow2-extract` on a signed guest image: the raw disk has the
//! image's virtual size, a protective MBR and a GPT header, and matches
//! `qemu-img convert` byte for byte when qemu-img is installed.
use crate::runner::{self, case};
use crate::support::{exec::which, hamn, tmp::TempDir};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

pub fn main(args: &[String]) -> ExitCode {
    let [image, filters @ ..] = args else {
        eprintln!("usage: hamn-dev test qcow2 SIGNED_GUEST_IMAGE [FILTER...]");
        return ExitCode::from(2);
    };
    let image = PathBuf::from(image);
    if !image.is_file() {
        eprintln!("qcow2: signed guest image fixture not found: {}", image.display());
        return ExitCode::FAILURE;
    }
    runner::run(
        "qcow2",
        "qcow2 extraction yields the signed image's raw disk",
        vec![case("extracted_disk_matches_the_image", move || extracted_disk_matches_the_image(&image))],
        filters,
    )
}

fn extracted_disk_matches_the_image(image: &Path) {
    let work = TempDir::new("hamn-qcow2-");
    let raw = work.path().join("disk.raw");
    let status = Command::new(hamn()).arg("qcow2-extract").arg(image).arg(&raw).status().expect("run hamn");
    assert!(status.success(), "qcow2-extract: {status}");

    // The qcow2 header stores the virtual size as a big-endian u64 at 24.
    let header = read_at(image, 0, 32);
    let virtual_size = u64::from_be_bytes(header[24..32].try_into().unwrap());
    assert_eq!(std::fs::metadata(&raw).unwrap().len(), virtual_size, "raw size differs from the virtual size");
    assert_eq!(read_at(&raw, 510, 2), [0x55, 0xaa], "no protective MBR boot signature");
    assert_eq!(read_at(&raw, 512, 8), b"EFI PART", "no GPT header at LBA 1");

    match which("qemu-img") {
        Some(qemu_img) => {
            let reference = work.path().join("reference.raw");
            let status =
                Command::new(qemu_img).args(["convert", "-O", "raw"]).arg(image).arg(&reference).status().unwrap();
            assert!(status.success(), "qemu-img convert: {status}");
            assert_eq!(sha256(&raw), sha256(&reference), "extracted disk differs from qemu-img's");
            eprintln!("qemu-img SHA-256 cross-check passed");
        }
        None => eprintln!("qemu-img unavailable; structural checks only"),
    }
}

fn read_at(path: &Path, offset: u64, length: usize) -> Vec<u8> {
    let mut file = File::open(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    file.seek(SeekFrom::Start(offset)).unwrap();
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes).unwrap_or_else(|error| panic!("{} at {offset}: {error}", path.display()));
    bytes
}

fn sha256(path: &Path) -> Vec<u8> {
    let mut file = File::open(path).unwrap();
    let mut hasher = Sha256::new();
    let mut block = vec![0; 1 << 20];
    loop {
        let count = file.read(&mut block).unwrap();
        if count == 0 {
            return hasher.finalize().to_vec();
        }
        hasher.update(&block[..count]);
    }
}
