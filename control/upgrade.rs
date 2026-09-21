//! Human upgrade aliases and post-TUI cached notices. Mutations use the same
//! service/worker contract as headless operations; this module never installs.
use crate::model::{Failure, Request};
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{IsTerminal, Write},
    os::fd::AsRawFd,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn is_command(args: &[OsString]) -> bool {
    args.get(1)
        .is_some_and(|arg| arg == "upgrade" || arg == "update")
}

pub fn run_cli(args: &[OsString]) -> i32 {
    let mut request = Request {
        words: vec!["system".into(), "update".into()],
        headless: true,
        yes: true, // Explicit `hamn upgrade` is the human mutation request.
        timeout: 600,
        tail: 200,
        ..Request::default()
    };
    let mut json = false;
    let parsed = (|| {
        let mut index = 2;
        while index < args.len() {
            let word = args[index].to_str().ok_or("invalid argument encoding")?;
            match word {
                "--check" if !request.check => request.check = true,
                "--force" if !request.force => request.force = true,
                "--manifest" if request.manifest.is_none() => {
                    index += 1;
                    request.manifest = Some(
                        args.get(index)
                            .and_then(|v| v.to_str())
                            .filter(|v| !v.is_empty())
                            .ok_or("--manifest requires a value")?
                            .into(),
                    );
                }
                "--output" if !json => {
                    index += 1;
                    if args.get(index).is_none_or(|v| v != "json") {
                        return Err("--output requires json");
                    }
                    json = true;
                }
                "--help" | "-h" => return Ok(true),
                _ => return Err("unknown or repeated upgrade option"),
            }
            index += 1;
        }
        if request.check && request.force {
            return Err("--check conflicts with --force");
        }
        Ok(false)
    })();
    match parsed {
        Ok(true) => {
            println!(
                "Usage: hamn upgrade [--check] [--force] [--manifest URL] [--output json]\n\nhamn update is an alias. --check reads only release metadata.\n--force reinstalls the same release; stable downgrades are rejected.\nOnly managed installations may be changed. Existing profile disks are preserved."
            );
            return 0;
        }
        Err(message) => {
            eprintln!("hamn upgrade: {message}");
            return 2;
        }
        _ => {}
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("upgrade runtime");
    let result = runtime.block_on(async {
        let cancel = tokio_util::sync::CancellationToken::new();
        let signal = cancel.clone();
        let listener = tokio::spawn(async move {
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("signal handler");
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            signal.cancel();
        });
        let result = crate::service::execute_stream(&request, &cancel, None).await;
        listener.abort();
        result
    });
    match result {
        Ok(value) => {
            if json {
                println!("{value}");
            } else {
                println!(
                    "Hamn {} → {}: {}. Downloaded {} bytes; reused {} bytes. Existing profile disks unchanged.",
                    value["currentVersion"].as_str().unwrap_or("unknown"),
                    value["latestVersion"].as_str().unwrap_or("unknown"),
                    value["status"].as_str().unwrap_or("completed"),
                    value["downloadedBytes"],
                    value["reusedBytes"]
                );
            }
            0
        }
        Err(Failure { code, message }) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"schemaVersion":1,"ok":false,"error":{"code":code,"message":message}})
                );
            } else {
                eprintln!("hamn upgrade: {message}");
            }
            1
        }
    }
}

fn stable(value: &str) -> Option<[u32; 3]> {
    let mut result = [0; 3];
    let parts: Vec<_> = value
        .strip_prefix('v')
        .unwrap_or(value)
        .split('.')
        .collect();
    if parts.len() != 3 {
        return None;
    }
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty()
            || (part.len() > 1 && part.starts_with('0'))
            || !part.bytes().all(|v| v.is_ascii_digit())
        {
            return None;
        }
        result[index] = part.parse().ok()?;
    }
    Some(result)
}

fn owned_regular(path: &Path, private: bool, limit: u64) -> Option<fs::Metadata> {
    let info = fs::symlink_metadata(path).ok()?;
    (info.is_file()
        && info.uid() == unsafe { libc::geteuid() }
        && info.nlink() == 1
        && info.len() <= limit
        && info.mode() & 0o022 == 0
        && (!private || info.permissions().mode() & 0o777 == 0o600))
        .then_some(info)
}

fn read_private(path: &Path) -> Option<serde_json::Value> {
    let expected = owned_regular(path, true, 4096)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let actual = file.metadata().ok()?;
    if actual.dev() != expected.dev() || actual.ino() != expected.ino() {
        return None;
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 4096 {
        return None;
    }
    // Both versioned cache contracts are flat objects. Reject duplicate keys
    // before Value deserialization can silently keep the last occurrence.
    struct Unique;
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = serde_json::Value;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("unique cache object")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut result = serde_json::Map::new();
            while let Some((key, value)) = map.next_entry::<String, serde_json::Value>()? {
                if result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate cache key"));
                }
            }
            Ok(serde_json::Value::Object(result))
        }
    }
    use serde::Deserializer;
    let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
    let value = deserializer.deserialize_map(Unique).ok()?;
    deserializer.end().ok()?;
    Some(value)
}

fn managed_helper() -> Option<(PathBuf, PathBuf)> {
    let executable = fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    let invocation = std::env::args_os().next()?;
    let invocation = if Path::new(&invocation).components().count() > 1 {
        PathBuf::from(invocation)
    } else {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(&invocation))
            .find(|path| path.exists())?
    };
    if !fs::symlink_metadata(&invocation)
        .ok()?
        .file_type()
        .is_symlink()
        || fs::canonicalize(invocation).ok()? != executable
    {
        return None;
    }
    let generation = executable.parent()?.parent()?;
    if executable.file_name()? != "hamn"
        || executable.parent()?.file_name()? != "bin"
        || generation.parent()?.file_name()? != ".hamn-generations"
    {
        return None;
    }
    let source = generation.join("share/hamn/src");
    let pointer = source.join("packaging/release/update-manifest-url");
    owned_regular(&executable, false, 128 * 1024 * 1024)?;
    owned_regular(&pointer, false, 4096)?;
    Some((executable, pointer))
}

/// Call only after a successful TUI exit, after its terminal guard is restored.
/// All errors are ignored: cached notices must not change the original result.
pub fn after_tui() {
    if !std::io::stdout().is_terminal()
        || !std::io::stderr().is_terminal()
        || std::env::var_os("CI").is_some()
        || std::env::var("HAMN_NO_UPDATE_CHECK").as_deref() == Ok("1")
    {
        return;
    }
    let Some(current) = stable(env!("HAMN_VERSION")) else {
        return;
    };
    let Some((helper, pointer)) = managed_helper() else {
        return;
    };
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let cache = home.join(".hamn/cache");
    for directory in [home.join(".hamn"), cache.clone()] {
        let info = match fs::symlink_metadata(directory) {
            Ok(info) => info,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return,
        };
        if !info.is_dir() || info.uid() != unsafe { libc::geteuid() } || info.mode() & 0o022 != 0 {
            return;
        }
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let record = read_private(&cache.join("update-check-v1.json")).filter(|record| {
        record.as_object().is_some_and(|v| {
            v.len() == 4
                && v.keys().all(|key| {
                    matches!(
                        key.as_str(),
                        "schemaVersion" | "checkedAt" | "ok" | "latestVersion"
                    )
                })
        }) && record["schemaVersion"] == 1
            && record["checkedAt"]
                .as_u64()
                .is_some_and(|stamp| stamp <= now)
            && record["ok"].is_boolean()
            && ((record["ok"] == false && record["latestVersion"].is_null())
                || record["latestVersion"].as_str().and_then(stable).is_some())
    });
    if let Some(record) = &record {
        if let Some(latest) = record["latestVersion"]
            .as_str()
            .filter(|value| stable(value).is_some_and(|v| v > current))
        {
            let notice_path = cache.join("update-notice-v1.json");
            let lock_path = cache.join(".update-notice.lock");
            let Ok(notice_lock) = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&lock_path)
            else {
                return;
            };
            if owned_regular(&lock_path, true, 0).is_none()
                || unsafe { libc::flock(notice_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }
                    != 0
            {
                return;
            }
            let due = read_private(&notice_path).is_none_or(|notice| {
                notice.as_object().is_none_or(|v| v.len() != 3)
                    || notice["schemaVersion"] != 1
                    || notice["latestVersion"] != latest
                    || notice["shownAt"]
                        .as_u64()
                        .is_none_or(|stamp| stamp > now || now - stamp >= 86400)
            });
            if due {
                // Exclusive creation + rename avoids following an existing link.
                let stage = cache.join(format!(".update-notice-{}-{now}", std::process::id()));
                if let Ok(mut file) = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&stage)
                {
                    let value =
                        serde_json::json!({"schemaVersion":1,"latestVersion":latest,"shownAt":now});
                    let safe_target = fs::symlink_metadata(&notice_path)
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                        || owned_regular(&notice_path, true, 4096).is_some();
                    if safe_target
                        && writeln!(file, "{value}").is_ok()
                        && file.sync_all().is_ok()
                        && fs::rename(&stage, &notice_path).is_ok()
                    {
                        eprintln!("Hamn {latest} is available; run hamn upgrade.");
                        if let Ok(directory) = fs::File::open(&cache) {
                            let _ = directory.sync_all();
                        }
                    }
                    let _ = fs::remove_file(stage);
                }
            }
        }
        let ttl = if record["ok"] == true { 86400 } else { 21600 };
        if now - record["checkedAt"].as_u64().unwrap_or(0) < ttl {
            return;
        }
    }
    let Ok(manifest) = fs::read_to_string(pointer) else {
        return;
    };
    if let Ok(mut child) =
        checker_command(&helper, &home, manifest.trim(), env!("HAMN_VERSION")).spawn()
    {
        // The scheduler exits after spawning a detached checker; wait/reap in
        // a helper thread, never wait for release network in the foreground.
        let _ = std::thread::Builder::new()
            .name("update-check-reaper".into())
            .spawn(move || {
                let _ = child.wait();
            });
    }
}

fn checker_command(executable: &Path, home: &Path, manifest: &str, current: &str) -> Command {
    let mut command = Command::new(executable);
    command
        .args(["__install-support", "upgrade", "schedule"])
        .arg("--manifest")
        .arg(manifest)
        .arg("--current-version")
        .arg(current)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[cfg(test)]
mod tests {
    use super::stable;
    #[test]
    fn scheduler_uses_native_binary_clean_environment_and_detached_stdio() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = crate::install_support::test_support::Temp::new();
        let script = temporary.0.join("record");
        let keys = temporary.0.join("keys");
        let args = temporary.0.join("args");
        std::fs::write(&script, format!("#!/bin/sh\nset -eu\numask 077\nfor fd in 0 1 2; do if test -t \"$fd\"; then exit 91; fi; done\n/usr/bin/env | /usr/bin/cut -d = -f 1 > '{}'\nprintf '%s\\n' \"$@\" > '{}'\ntest \"$HOME\" = '{}'\ntest \"$PATH\" = /usr/bin:/bin:/usr/sbin:/sbin\n", keys.display(), args.display(), temporary.0.display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = super::checker_command(
            &script,
            &temporary.0,
            "https://fixture.invalid/manifest",
            "1.2.3",
        );
        assert!(command.status().unwrap().success());
        let keys = std::fs::read_to_string(keys).unwrap();
        assert!(
            keys.lines()
                .all(|key| ["HOME", "PATH", "PWD", "SHLVL", "_"].contains(&key)),
            "unexpected inherited environment key"
        );
        assert_eq!(
            std::fs::read_to_string(args).unwrap(),
            "__install-support\nupgrade\nschedule\n--manifest\nhttps://fixture.invalid/manifest\n--current-version\n1.2.3\n"
        );
    }
    #[test]
    fn stable_versions_reject_metadata_and_overflow() {
        assert_eq!(stable("v4294967295.2.3"), Some([u32::MAX, 2, 3]));
        for value in [
            "1.2",
            "01.2.3",
            "1.2.3-rc.1",
            "1.2.3+build",
            "4294967296.0.0",
            " 1.2.3",
        ] {
            assert_eq!(stable(value), None);
        }
        assert!(stable("1.10.0") > stable("1.9.99"));
    }
}
