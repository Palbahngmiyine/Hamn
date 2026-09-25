//! The real build script (build.rs, compiled with rustc) propagates the
//! selected Apple SDK to the final linker as a separate argument and passes
//! the release version to the C core. Only native compilation is stubbed:
//! `make` records its arguments and `clang` names a compiler runtime.
use crate::runner::{self, case};
use crate::support::tmp::TempDir;
use crate::support::tui::install_fixture;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output};

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "rust-sdk",
        "build.rs passes the selected SDK to the linker and the release version to the C core",
        // unittest's order: test methods sorted by name.
        vec![
            case(
                "test_direct_cargo_build_uses_release_version_and_explicit_override",
                test_direct_cargo_build_uses_release_version_and_explicit_override,
            ),
            case("test_selected_sdk_is_a_separate_linker_argument", test_selected_sdk_is_a_separate_linker_argument),
        ],
        filters,
    )
}

type Environment = BTreeMap<OsString, OsString>;

/// Compiles build.rs from the working directory (the checkout) into
/// `root/build-script`, links `make` and `clang` fixtures into `root`, and
/// returns the script with the environment Cargo would give it.
fn prepare(root: &Path, manifest_dir: &Path, runtime: &Path) -> (PathBuf, Environment) {
    let script = root.join("build-script");
    let status = Command::new("rustc").arg("build.rs").arg("-o").arg(&script).status().expect("rustc");
    assert!(status.success(), "rustc build.rs: {status}");
    for name in ["make", "clang"] {
        install_fixture(root, name);
    }
    let mut environment: Environment = std::env::vars_os().collect();
    let path = environment.get(&OsString::from("PATH")).cloned().expect("PATH");
    let mut search = root.as_os_str().to_owned();
    search.push(":");
    search.push(path);
    for (key, value) in [
        ("PATH", search),
        ("CARGO_CFG_TARGET_OS", "macos".into()),
        ("CARGO_MANIFEST_DIR", manifest_dir.into()),
        ("OUT_DIR", root.join("out").into()),
        ("TEST_RUNTIME", runtime.into()),
        ("TEST_MAKE_ARGS", root.join("make-args").into()),
        ("HAMN_DEV_FIXTURE", "rust-sdk".into()),
    ] {
        environment.insert(key.into(), value);
    }
    environment.remove(&OsString::from("HAMN_VERSION"));
    (script, environment)
}

fn run(script: &Path, environment: &Environment) -> Output {
    Command::new(script).env_clear().envs(environment).output().expect("run the build script")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("UTF-8 output")
}

fn test_selected_sdk_is_a_separate_linker_argument() {
    let directory = TempDir::new_in(&std::env::temp_dir(), "hamn-sdk-");
    let root = directory.path();
    let sdk = root.join("Apple SDK with spaces");
    fs::create_dir(&sdk).unwrap();
    let runtime = root.join("libclang_rt.osx.a");
    fs::write(&runtime, "").unwrap();
    let checkout = std::env::current_dir().unwrap();
    let (script, environment) = prepare(root, &checkout, &runtime);
    let sdk = sdk.to_str().unwrap().to_owned();
    let missing = root.join("missing").to_str().unwrap().to_owned();
    let wrong = root.join("wrong-nix-sdk").to_str().unwrap().to_owned();
    let some = |value: &str| Some(value.to_owned());
    let mut cases: Vec<(Option<String>, Option<String>)> =
        [some(&sdk), some(""), None, some(&missing), some("relative-sdk")].into_iter().map(|selected| (selected, None)).collect();
    // A toolchain wrapper may reset SDKROOT; the runner's explicit SDK wins.
    cases.extend([(some(&wrong), some(&sdk)), (None, some(&sdk)), (some(&sdk), some("")), (some(&sdk), some(&missing))]);
    let version = fs::read_to_string("version.txt").unwrap().trim().to_owned();
    let mut failures = Vec::new();
    for (selected, preserved) in cases {
        // Python's `preserved or selected`.
        let effective = preserved.clone().filter(|value| !value.is_empty()).or_else(|| selected.clone());
        let subtest = format!("sdk={selected:?}, preserved={preserved:?}");
        let outcome = std::panic::catch_unwind(|| {
            let mut current = environment.clone();
            current.remove(&OsString::from("SDKROOT"));
            current.remove(&OsString::from("HAMN_SYSTEM_SDKROOT"));
            if let Some(selected) = &selected {
                current.insert("SDKROOT".into(), selected.into());
            }
            if let Some(preserved) = &preserved {
                current.insert("HAMN_SYSTEM_SDKROOT".into(), preserved.into());
            }
            let result = run(&script, &current);
            let (stdout, stderr) = (text(&result.stdout), text(&result.stderr));
            let selected_sdk = effective.as_deref().is_some_and(|value| !value.is_empty());
            if effective.as_deref() == Some(missing.as_str()) || effective.as_deref() == Some("relative-sdk") {
                assert!(!result.status.success(), "{stdout}");
                assert!(stderr.contains("SDKROOT must name"), "{stderr}");
            } else {
                assert!(result.status.success(), "{stderr}");
                let args: Vec<&str> = stdout.lines().filter_map(|line| line.strip_prefix("cargo:rustc-link-arg=")).collect();
                let expected: Vec<&str> = if selected_sdk { vec!["-isysroot", &sdk] } else { Vec::new() };
                assert_eq!(args, expected);
                let native = fs::read_to_string(root.join("make-args")).unwrap();
                let native_args: Vec<&str> = native.lines().collect();
                // Rust reads the version from the C core at run time.
                assert!(native_args.contains(&format!("VERSION={version}").as_str()), "{native_args:?}");
                assert!(!stdout.contains("cargo:rustc-env=HAMN_VERSION"), "{stdout}");
                let sdk_args: Vec<&str> = native_args.iter().copied().filter(|arg| arg.starts_with("SDKROOT=")).collect();
                let expected = format!("SDKROOT={sdk}");
                assert_eq!(sdk_args, if selected_sdk { vec![expected.as_str()] } else { Vec::new() });
            }
        });
        if outcome.is_err() {
            failures.push(subtest);
        }
    }
    assert!(failures.is_empty(), "failed subtests: {failures:?}");
}

fn test_direct_cargo_build_uses_release_version_and_explicit_override() {
    let directory = TempDir::new_in(&std::env::temp_dir(), "hamn-version-");
    let root = directory.path();
    fs::write(root.join("version.txt"), "0.1.0\n").unwrap();
    let runtime = root.join("runtime.a");
    fs::write(&runtime, "").unwrap();
    let (script, mut environment) = prepare(root, root, &runtime);
    let make_args = root.join("make-args");
    for key in ["HAMN_VERSION", "SDKROOT", "HAMN_SYSTEM_SDKROOT"] {
        environment.remove(&OsString::from(key));
    }
    let succeed = |environment: &Environment| {
        let result = run(&script, environment);
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        text(&result.stdout)
    };
    let native_args = || fs::read_to_string(&make_args).unwrap().lines().map(str::to_owned).collect::<Vec<_>>();
    let stdout = succeed(&environment);
    assert!(native_args().contains(&"VERSION=0.1.0".to_owned()), "{:?}", native_args());
    assert!(stdout.contains("cargo:rerun-if-changed=version.txt"), "{stdout}");
    assert!(stdout.contains("cargo:rerun-if-env-changed=HAMN_VERSION"), "{stdout}");
    environment.insert("HAMN_VERSION".into(), "0.1.0-dev".into());
    succeed(&environment);
    assert!(native_args().contains(&"VERSION=0.1.0-dev".to_owned()), "{:?}", native_args());
}

/// The former /bin/sh stubs: `make` writes its arguments, one per line, to
/// `$TEST_MAKE_ARGS` (`printf "%s\n" "$@"`), and `clang` prints
/// `$TEST_RUNTIME`.
pub fn fixture(program: &str, args: &[String]) -> ExitCode {
    match program {
        "make" => {
            // printf repeats its format for each argument, and applies it
            // once (to an empty argument) when there is none.
            let lines: String =
                if args.is_empty() { "\n".to_owned() } else { args.iter().map(|arg| format!("{arg}\n")).collect() };
            let target = std::env::var_os("TEST_MAKE_ARGS").unwrap_or_default();
            match fs::write(&target, lines) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("make fixture: {}: {error}", Path::new(&target).display());
                    ExitCode::FAILURE
                }
            }
        }
        "clang" => {
            use std::io::Write;
            use std::os::unix::ffi::OsStrExt;
            let mut line = std::env::var_os("TEST_RUNTIME").unwrap_or_default().as_bytes().to_vec();
            line.push(b'\n');
            std::io::stdout().write_all(&line).expect("write standard output");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("rust-sdk fixture: unexpected program {program}");
            ExitCode::from(127)
        }
    }
}
