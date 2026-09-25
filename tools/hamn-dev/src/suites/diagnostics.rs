//! `vm diagnostics` archives: private, atomic, never replacing existing files
//! or following symlinks, bounded, and free of credentials. Each case uses a
//! fresh HOME with one created (never started) profile whose logs and files
//! plant credential canaries.
use crate::runner::{self, case};
use crate::support::{hamn, tmp::TempDir};
use regex::bytes::Regex;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output};

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "diagnostics",
        "diagnostic archives are bounded, atomic, and credential-redacted",
        vec![
            case("archive_is_private_complete_and_redacted", archive_is_private_complete_and_redacted),
            case("existing_files_and_symlinks_are_never_replaced", existing_files_and_symlinks_are_never_replaced),
            case("bounded_tail_drops_its_partial_first_line", bounded_tail_drops_its_partial_first_line),
            case("log_symlinks_are_not_followed", log_symlinks_are_not_followed),
            case("default_archive_is_private_below_hamn", default_archive_is_private_below_hamn),
            case("missing_path_value_is_an_invalid_request", missing_path_value_is_an_invalid_request),
            case("leak_detector_reports_each_credential_kind", leak_detector_reports_each_credential_kind),
        ],
        filters,
    )
}

const TOKEN: &str = "hamn-diagnostic-token-canary-9f31e7";
const KEY: &str = "HamnPrivateKeyPayloadCanary9f31e7";
const SECRET: &str = "hamn-arbitrary-secret-data-canary-9f31e7";
const KUBECONFIG: &str = "hamn-kubeconfig-file-canary-9f31e7";
const BOUNDARY: &str = "hamn-tail-boundary-canary-value";
const SYMLINK: &str = "hamn-symlink-log-canary-value";
const MULTILINE: &str = "hamn-multiline-credential-canary-value";
const DIRECTORY_SYMLINK: &str = "hamn-log-directory-symlink-canary-value";
const ANSI: &str = "hamn-ansi-obfuscated-token-canary-value";
const JSON_DATA: &str = "hamn-json-data-before-kind-canary-value";
const PRIVATE_BLOCK: &str = "HamnIndependentPrivateBlockCanary9f31e7";
const OPAQUE_HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const OPAQUE_BASE64URL: &str = "abcdefghijklmnopqrstuvwxyz0123456789_-abcdefghijklmnop";
const OPAQUE_SHORT: &str = "ghijklmnopqrstuvwxyz0123456789abcdefgh";
const BOOTSTRAP_TOKEN: &str = "abcdef.0123456789abcdef";
const CANARIES: &[&str] = &[
    TOKEN,
    KEY,
    SECRET,
    KUBECONFIG,
    BOUNDARY,
    SYMLINK,
    MULTILINE,
    DIRECTORY_SYMLINK,
    ANSI,
    JSON_DATA,
    PRIVATE_BLOCK,
    OPAQUE_HEX,
    OPAQUE_BASE64URL,
    OPAQUE_SHORT,
    BOOTSTRAP_TOKEN,
];

/// A HOME with a created `default` profile, a kubeconfig and a private key
/// file holding canaries, and canary-bearing serial and vmrun logs.
struct Home {
    directory: TempDir,
}

impl Home {
    fn new() -> Self {
        let home = Self { directory: TempDir::new("hamn-diagnostics-") };
        let created = home.hamn(&["vm", "create", "--profile", "default", "--yes"]);
        assert!(created.status.success(), "{created:?}");
        fs::create_dir_all(home.logs()).unwrap();
        fs::create_dir_all(home.path().join(".kube")).unwrap();
        private_file(
            &home.path().join(".kube/config"),
            &format!(
                "apiVersion: v1\nkind: Config\nclusters: []\nusers:\n- name: diagnostic-test\n  user:\n    token: {KUBECONFIG}\ncurrent-context: diagnostic-test\n"
            ),
        );
        private_file(
            &home.profile().join("private-key.pem"),
            &format!("-----BEGIN PRIVATE KEY-----\n{KEY}\n-----END PRIVATE KEY-----\n"),
        );
        private_file(
            &home.logs().join("serial.log"),
            &format!(
                "serial console initialized safely\nserial console resumed safely\n{OPAQUE_HEX}\n{OPAQUE_BASE64URL}\n{OPAQUE_SHORT}\n\
                 bootstrap handshake {BOOTSTRAP_TOKEN} accepted\n\
                 bootstrap handshake abcde.1123456789abcdef preserved\n\
                 bootstrap handshake ghijkl.0123456789abcde preserved\n\
                 bootstrap handshake abcdefg.1123456789abcdef preserved\n\
                 bootstrap handshake ghijkl.0123456789abcdef0 preserved\n\
                 token: {TOKEN}\n-----BEGIN PRIVATE KEY-----\n{KEY}\n-----END PRIVATE KEY-----\n\
                 password:\n  {MULTILINE}\nserial console resumed after redaction safely\n"
            ),
        );
        private_file(
            &home.logs().join("vmrun.log"),
            &format!(
                "vmrun started safely\nto\x1b[31mken: {ANSI}\nAuthorization: Bearer {TOKEN}\nenv:\n  - name: arbitrary\n    value: {SECRET}\n\
                 kind: Secret\ndata:\n  arbitrary-name: {SECRET}\nvmrun completed after redaction safely\n"
            ),
        );
        home
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn profile(&self) -> PathBuf {
        self.path().join(".hamn/default")
    }

    fn logs(&self) -> PathBuf {
        self.profile().join("logs")
    }

    fn hamn(&self, arguments: &[&str]) -> Output {
        Command::new(hamn()).env("HOME", self.path()).arg("--headless").args(arguments).output().expect("run hamn")
    }

    fn diagnostics(&self, path: &Path) -> Output {
        let path = path.to_str().unwrap();
        self.hamn(&["vm", "diagnostics", "--profile", "default", "--yes", "--path", path])
    }

    /// Creates an archive at `name`, extracts it beside itself and checks
    /// that the extracted bundle leaks no credential or canary.
    fn extracted(&self, name: &str) -> PathBuf {
        let archive = self.path().join(format!("{name}.tar"));
        let result = self.diagnostics(&archive);
        assert!(result.status.success(), "{result:?}");
        let directory = self.path().join(name);
        fs::create_dir(&directory).unwrap();
        run(Command::new("/usr/bin/tar").arg("-xf").arg(&archive).arg("-C").arg(&directory));
        assert_redacted(&directory);
        directory
    }
}

fn private_file(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn run(command: &mut Command) -> String {
    let output = command.output().unwrap_or_else(|error| panic!("{command:?}: {error}"));
    assert!(output.status.success(), "{command:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path).unwrap().lines().map(str::to_owned).collect()
}

fn has_line(path: &Path, line: &str) -> bool {
    lines(path).iter().any(|actual| actual == line)
}

fn archive_is_private_complete_and_redacted() {
    let home = Home::new();
    let archive = home.path().join("output with spaces/nested/diagnostic archive.tar");
    let result = home.diagnostics(&archive);
    assert!(result.status.success(), "{result:?}");
    assert!(result.stderr.is_empty(), "{}", String::from_utf8_lossy(&result.stderr));
    let text = String::from_utf8(result.stdout).unwrap();
    for field in [r#""schemaVersion":1"#, r#""operation":"diagnostics.create""#, r#""format":"ustar""#, r#""redacted":true"#] {
        assert!(text.contains(field), "{field}: {text}");
    }
    assert!(text.contains(archive.to_str().unwrap()), "{text}");
    assert!(archive.is_file());
    assert_eq!(mode(&archive), 0o600);
    assert_eq!(mode(archive.parent().unwrap()), 0o700);
    let temporary = fs::read_dir(archive.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp."))
        .collect::<Vec<_>>();
    assert!(temporary.is_empty(), "diagnostic creation left a temporary archive: {temporary:?}");

    let mut members: Vec<String> =
        run(Command::new("/usr/bin/tar").arg("-tf").arg(&archive)).lines().map(str::to_owned).collect();
    members.sort();
    assert_eq!(members, ["logs/serial.log", "logs/vmrun.log", "manifest.json", "status.json"]);

    let extracted = home.path().join("extracted");
    fs::create_dir(&extracted).unwrap();
    run(Command::new("/usr/bin/tar").arg("-xf").arg(&archive).arg("-C").arg(&extracted));
    assert_redacted(&extracted);
    let serial = extracted.join("logs/serial.log");
    let vmrun = extracted.join("logs/vmrun.log");
    for line in [
        "serial console initialized safely",
        "serial console resumed safely",
        "serial console resumed after redaction safely",
        "bootstrap handshake abcde.1123456789abcdef preserved",
        "bootstrap handshake ghijkl.0123456789abcde preserved",
        "bootstrap handshake abcdefg.1123456789abcdef preserved",
        "bootstrap handshake ghijkl.0123456789abcdef0 preserved",
        "[REDACTED sensitive log line]",
    ] {
        assert!(has_line(&serial, line), "serial.log lacks {line:?}: {:?}", lines(&serial));
    }
    for line in ["vmrun started safely", "vmrun completed after redaction safely"] {
        assert!(has_line(&vmrun, line), "vmrun.log lacks {line:?}: {:?}", lines(&vmrun));
    }
    let status = fs::read_to_string(extracted.join("status.json")).unwrap();
    assert!(status.contains(r#""schemaVersion":1"#) && status.contains(r#""dockerContext":"hamn""#), "{status}");
    let manifest = fs::read_to_string(extracted.join("manifest.json")).unwrap();
    assert!(manifest.contains(r#""collectionPolicy":"allowlisted metadata and bounded log tails""#), "{manifest}");
}

fn existing_files_and_symlinks_are_never_replaced() {
    let home = Home::new();
    let archive = home.path().join("archive.tar");
    assert!(home.diagnostics(&archive).status.success());
    let digest = Sha256::digest(fs::read(&archive).unwrap());
    let existing = home.diagnostics(&archive);
    assert!(!existing.status.success(), "diagnostics replaced an existing archive");
    assert!(String::from_utf8_lossy(&existing.stdout).contains("cannot create diagnostic archive"), "{existing:?}");
    assert_eq!(Sha256::digest(fs::read(&archive).unwrap()), digest);

    let outside = home.path().join("outside");
    fs::write(&outside, "outside-unchanged\n").unwrap();
    let link = home.path().join("output-link.tar");
    symlink(&outside, &link).unwrap();
    assert!(!home.diagnostics(&link).status.success(), "diagnostics replaced an output symlink");
    assert_eq!(fs::read_to_string(&outside).unwrap(), "outside-unchanged\n");
}

/// A field name before the tail's read window must not leave its value
/// behind: the partial first line of a bounded tail is dropped. A JSON
/// Secret's data before its `kind` is also redacted.
fn bounded_tail_drops_its_partial_first_line() {
    let home = Home::new();
    let mut serial = b"token: ".to_vec();
    serial.extend(std::iter::repeat_n(b'x', 140 * 1024));
    serial.extend(format!("{BOUNDARY}\ntail boundary completed safely\n").as_bytes());
    fs::write(home.logs().join("serial.log"), serial).unwrap();
    fs::write(
        home.logs().join("vmrun.log"),
        format!(
            "vmrun JSON diagnostic line safely\n{{\n  \"data\" : {{\n    \"opaque\": \"{JSON_DATA}\"\n  }},\n  \"kind\": \"Secret\"\n}}\n"
        ),
    )
    .unwrap();
    let extracted = home.extracted("boundary");
    assert!(has_line(&extracted.join("logs/serial.log"), "tail boundary completed safely"));
}

fn log_symlinks_are_not_followed() {
    let home = Home::new();
    let outside = home.path().join("outside-secret-log");
    fs::write(&outside, format!("{SYMLINK}\n")).unwrap();
    fs::remove_file(home.logs().join("serial.log")).unwrap();
    symlink(&outside, home.logs().join("serial.log")).unwrap();
    fs::write(
        home.logs().join("vmrun.log"),
        format!(
            "vmrun private block diagnostic line safely\n-----BEGIN PRIVATE KEY-----\n{PRIVATE_BLOCK}\n-----END PRIVATE KEY-----\nvmrun after private block safely\n"
        ),
    )
    .unwrap();
    let extracted = home.extracted("symlink-log");
    assert!(has_line(&extracted.join("logs/serial.log"), "(log unavailable)"));
    assert!(has_line(&extracted.join("logs/vmrun.log"), "vmrun after private block safely"));

    // An intermediate logs-directory symlink is also rejected.
    fs::remove_dir_all(home.logs()).unwrap();
    let outside_logs = home.path().join("outside-logs");
    fs::create_dir(&outside_logs).unwrap();
    for name in ["serial.log", "vmrun.log"] {
        fs::write(outside_logs.join(name), format!("{DIRECTORY_SYMLINK}\n")).unwrap();
    }
    symlink(&outside_logs, home.logs()).unwrap();
    let extracted = home.extracted("symlink-log-directory");
    for name in ["serial.log", "vmrun.log"] {
        assert!(has_line(&extracted.join("logs").join(name), "(log unavailable)"), "{name}");
    }
}

fn default_archive_is_private_below_hamn() {
    let home = TempDir::new("hamn-diagnostics-default-");
    let hamn = |arguments: &[&str]| {
        Command::new(hamn()).env("HOME", home.path()).arg("--headless").args(arguments).output().expect("run hamn")
    };
    assert!(hamn(&["vm", "create", "--profile", "default", "--yes"]).status.success());
    let result = hamn(&["vm", "diagnostics", "--profile", "default", "--yes"]);
    assert!(result.status.success(), "{result:?}");
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let path = PathBuf::from(value["data"]["path"].as_str().unwrap_or_else(|| panic!("no archive path: {value}")));
    assert!(
        path.starts_with(home.path().join(".hamn/diagnostics")) && path.extension().is_some_and(|extension| extension == "tar"),
        "default diagnostic path escaped ~/.hamn/diagnostics: {}",
        path.display()
    );
    assert!(path.is_file());
    assert_eq!(mode(&path), 0o600);
}

fn missing_path_value_is_an_invalid_request() {
    let home = Home::new();
    let result = home.hamn(&["vm", "diagnostics", "--profile", "default", "--yes", "--path"]);
    assert!(!result.status.success(), "diagnostics accepted a missing --path value");
    assert!(String::from_utf8_lossy(&result.stdout).contains(r#""code":"invalidRequest""#), "{result:?}");
}

/// Credential shapes that must not appear in any line of a diagnostic file,
/// as extended regular expressions matched line by line (like grep -E).
fn leak_patterns() -> Vec<(&'static str, Regex)> {
    let pattern = |category, expression: &str| (category, Regex::new(&format!("(?-u){expression}")).unwrap());
    vec![
        pattern("a private key", r"-----BEGIN ([A-Z0-9]+ )?PRIVATE KEY-----"),
        pattern(
            "a bearer credential",
            r"(^|[^[:alnum:]_])(Authorization:[[:space:]]*Bearer|Bearer)[[:space:]]+[A-Za-z0-9._~+/-]{8,}",
        ),
        pattern(
            "a credential field",
            r#"(^|[^[:alnum:]_])(token|access[_-]?token|refresh[_-]?token|password|client[-_]?key(-data)?|clientKeyData)[[:space:]"]*[:=][[:space:]]*"?[A-Za-z0-9+/_.=-]{4,}"#,
        ),
        pattern("an AWS access key", r"(^|[^A-Z0-9])(AKIA|ASIA)[A-Z0-9]{16}([^A-Z0-9]|$)"),
        pattern(
            "a JWT-like token",
            r"(^|[^A-Za-z0-9_-])[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}([^A-Za-z0-9_-]|$)",
        ),
        pattern("an opaque hexadecimal credential", r"^[[:space:]]*[[:xdigit:]]{32,}[[:space:]]*$"),
        pattern("an opaque encoded credential", r"^[[:space:]]*[A-Za-z0-9_+/-]{32,}={0,2}[[:space:]]*$"),
    ]
}

/// The leaks found in one file's contents: credential shapes, an embedded
/// kubeconfig (kind: Config with clusters: and users:), and canaries.
fn leaks(contents: &[u8]) -> Vec<&'static str> {
    let file_lines: Vec<&[u8]> = contents.split(|byte| *byte == b'\n').collect();
    let any = |regex: &Regex| file_lines.iter().any(|line| regex.is_match(line));
    let mut found: Vec<&'static str> =
        leak_patterns().into_iter().filter(|(_, regex)| any(regex)).map(|(category, _)| category).collect();
    let kubeconfig = [r"^[[:space:]]*kind:[[:space:]]*Config[[:space:]]*$", r"^[[:space:]]*clusters:[[:space:]]*$", r"^[[:space:]]*users:[[:space:]]*$"];
    if kubeconfig.iter().all(|expression| any(&Regex::new(&format!("(?-u){expression}")).unwrap())) {
        found.push("an embedded kubeconfig");
    }
    if CANARIES.iter().any(|canary| contents.windows(canary.len()).any(|window| window == canary.as_bytes())) {
        found.push("a secret canary");
    }
    found
}

/// An extracted bundle has at least one file, no symlinks, and no leaks.
fn assert_redacted(root: &Path) {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let kind = fs::symlink_metadata(&path).unwrap().file_type();
            assert!(!kind.is_symlink(), "diagnostic bundle contains a symlink: {}", path.display());
            if kind.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    assert!(!files.is_empty(), "diagnostic bundle is empty");
    files.sort();
    let reported: Vec<String> = files
        .iter()
        .flat_map(|file| {
            let relative = file.strip_prefix(root).unwrap().display().to_string();
            leaks(&fs::read(file).unwrap()).into_iter().map(move |category| format!("{category} in {relative}"))
        })
        .collect();
    assert!(reported.is_empty(), "diagnostic bundle contains {}", reported.join(", "));
}

/// The detector itself: each credential shape is reported, a multi-line
/// value is not joined across lines, and ordinary log lines are clean.
fn leak_detector_reports_each_credential_kind() {
    let cases: &[(&str, &str)] = &[
        ("-----BEGIN RSA PRIVATE KEY-----\n", "a private key"),
        ("Authorization: Bearer abcdefgh\n", "a bearer credential"),
        ("refresh_token=\"abcd\"\n", "a credential field"),
        ("key AKIAABCDEFGHIJKLMNOP used\n", "an AWS access key"),
        ("jwt aaaaaaaaaaaaaaaa.bbbbbbbbbbbbbbbb.cccccccccccccccc\n", "a JWT-like token"),
        ("  0123456789abcdef0123456789ABCDEF  \n", "an opaque hexadecimal credential"),
        ("abcdefghijklmnopqrstuvwxyz0123456789+/==\n", "an opaque encoded credential"),
        ("kind: Config\nclusters:\nusers:\n", "an embedded kubeconfig"),
        (TOKEN, "a secret canary"),
    ];
    for (contents, category) in cases {
        assert!(leaks(contents.as_bytes()).contains(category), "{category} not reported for {contents:?}");
    }
    for clean in [
        "serial console initialized safely\n",
        "token:\n  x\n",
        "bootstrap handshake abcde.1123456789abcdef preserved\n",
        "kind: Config\nclusters:\n",
        "[REDACTED sensitive log line]\n",
    ] {
        assert!(leaks(clean.as_bytes()).is_empty(), "{clean:?}: {:?}", leaks(clean.as_bytes()));
    }
}
