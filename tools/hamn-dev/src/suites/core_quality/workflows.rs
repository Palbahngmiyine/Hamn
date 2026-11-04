//! GitHub workflow contracts: the workflow set and its pinned, allowed
//! actions; PR CI running every macOS shard as one required check inside
//! the flake shell; Release Please; and the release workflow's automated,
//! GitHub-hosted, keyless and immutable promotion.
use super::search::{any_line_matches, assert_absent, has_line, lines, read};
use crate::support::exec::output_within;
use crate::support::tmp::TempDir;
use crate::support::upgrade::write_executable;
use regex::Regex;
use serde_yaml::{Mapping, Value};
use std::fs;
use std::path::Path;
use std::process::{Command, ExitStatus, Output};
use std::time::Duration;

const WORKFLOWS: &str = ".github/workflows";
const CI: &str = ".github/workflows/ci.yml";
const RELEASE_PLEASE: &str = ".github/workflows/release-please.yml";
const RELEASE: &str = ".github/workflows/release.yml";

/// The shell glob `.github/workflows/*.yml`: non-hidden names ending in
/// `.yml` directly in the directory, sorted.
fn workflow_files() -> Vec<String> {
    let mut files: Vec<String> = fs::read_dir(WORKFLOWS)
        .unwrap_or_else(|error| panic!("{WORKFLOWS}: {error} (run from the repository root)"))
        .map(|entry| entry.expect("workflow directory entry").file_name().into_string().expect("UTF-8 workflow name"))
        .filter(|name| !name.starts_with('.') && name.ends_with(".yml"))
        .map(|name| format!("{WORKFLOWS}/{name}"))
        .collect();
    files.sort();
    files
}

/// `make -s --no-print-directory TARGET` in the repository root.
fn make(target: &str) -> Output {
    output_within(Command::new("make").args(["-s", "--no-print-directory", target]), Duration::from_secs(60))
}

/// The lines of a job section as `awk '/^START$/ { capture = 1 } /^STOP$/ {
/// capture = 0 } capture { print }'` prints them: from each `start` line
/// (inclusive) up to the next `stop` line (exclusive).
fn section<'a>(text: &'a str, start: &str, stop: Option<&str>) -> Vec<&'a str> {
    let mut capture = false;
    let mut section = Vec::new();
    for line in lines(text) {
        if line == start {
            capture = true;
        }
        if stop == Some(line) {
            capture = false;
        }
        if capture {
            section.push(line);
        }
    }
    section
}

fn assert_lines(text: &str, requirements: &[&str], meaning: &str) {
    for requirement in requirements {
        assert!(has_line(text, requirement), "{meaning}: {requirement}");
    }
}

fn assert_section_lines(section: &[&str], requirements: &[&str], meaning: &str) {
    for requirement in requirements {
        assert!(section.contains(requirement), "{meaning}: {requirement}");
    }
}

pub fn release_workflow_is_keyless_and_least_privilege() {
    let release = read(RELEASE);
    assert!(release.contains("environment: hamn-promotion"), "promotion environment missing");
    assert!(has_line(&release, "  contents: read"), "release workflow must default to read-only repository contents");
    assert!(
        has_line(&release, "      contents: write"),
        "stable promotion must explicitly request repository write access"
    );
    assert_absent(
        r"HAMN_(VALIDATOR|RELEASE)_SIGNING_KEY|vars\.HAMN_RELEASE_PUBLIC_KEY|secrets\.HAMN_",
        &[RELEASE],
        "keyless release workflow still references long-lived signing material",
    );
}

/// `find .github/workflows -type f -name '*.yml'`: regular files (not
/// symbolic links) at any depth, hidden ones included.
pub fn workflow_set_is_ci_release_please_and_release() {
    fn walk(directory: &Path, found: &mut Vec<String>) {
        for entry in fs::read_dir(directory).unwrap_or_else(|error| panic!("{}: {error}", directory.display())) {
            let path = entry.expect("workflow directory entry").path();
            let kind =
                fs::symlink_metadata(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display())).file_type();
            let name = path.file_name().and_then(|name| name.to_str()).expect("UTF-8 workflow name");
            if kind.is_dir() {
                walk(&path, found);
            } else if kind.is_file() && name.ends_with(".yml") {
                found.push(path.to_str().expect("UTF-8 workflow path").to_owned());
            }
        }
    }
    let mut found = Vec::new();
    walk(Path::new(WORKFLOWS), &mut found);
    found.sort();
    assert_eq!(found, [CI, RELEASE_PLEASE, RELEASE], "workflow set must contain only CI, Release Please, and release");
}

/// The repository runs only GitHub-owned actions and this allowlist; any
/// other action fails the whole run before a job starts (startup_failure).
pub fn workflow_actions_are_pinned_and_allowed() {
    let uses = Regex::new(r"^[[:space:]]*uses:[[:space:]]*([^ #]+)").unwrap();
    let pinned = Regex::new(r"^[^@]+@[0-9a-f]{40}$").unwrap();
    for file in workflow_files() {
        let text = read(&file);
        for line in lines(&text) {
            let Some(captures) = uses.captures(line) else { continue };
            let action = &captures[1];
            assert!(pinned.is_match(action), "workflow action is not pinned to a full commit SHA: {action}");
            let name = action.split('@').next().unwrap();
            let allowed = name.starts_with("actions/")
                || name.starts_with("github/")
                || name == "cachix/install-nix-action"
                || name == "googleapis/release-please-action";
            assert!(allowed, "workflow action is not allowed by the repository Actions policy: {action}");
        }
    }
}

pub fn ci_macos_shards_partition_local_gates() {
    let output = make("check-ci-macos-shards");
    assert!(
        output.status.success(),
        "CI macOS shards do not cover the local macOS gates exactly once: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Ruby's `to_s` of a YAML scalar; `None` for a collection, which never
/// equals a shard name.
fn ruby_to_s(value: &Value) -> Option<String> {
    match value {
        Value::Null => Some(String::new()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::String(value) => Some(value.clone()),
        Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => None,
    }
}

fn required(value: &Value, key: &str, context: &str) -> Value {
    value.get(key).cloned().unwrap_or_else(|| panic!("{CI}: {context} has no {key}"))
}

/// PR CI runs every shard of `make print-ci-macos-shards` without path
/// filters, and one required check fails unless every shard passed.
pub fn ci_workflow_requires_every_shard() {
    let listed = make("print-ci-macos-shards");
    assert!(listed.status.success(), "make print-ci-macos-shards: {}", String::from_utf8_lossy(&listed.stderr));
    let shards: Vec<String> =
        String::from_utf8(listed.stdout).expect("UTF-8 shard list").split_whitespace().map(str::to_owned).collect();
    let mut workflow: Value = serde_yaml::from_str(&read(CI)).unwrap_or_else(|error| panic!("{CI}: {error}"));
    workflow.apply_merge().unwrap_or_else(|error| panic!("{CI}: {error}"));
    // YAML 1.1 parsers read the `on` key as the boolean true.
    let on = workflow.get("on").or_else(|| workflow.get(Value::Bool(true))).expect("CI workflow has no on");
    let trigger = on.get("pull_request").and_then(Value::as_mapping).expect("CI workflow has no pull_request trigger");
    assert!(
        !trigger.contains_key("paths") && !trigger.contains_key("paths-ignore"),
        "required PR checks must not be filtered by paths"
    );
    let jobs = required(&workflow, "jobs", "the workflow");
    let shard = required(&jobs, "macos-shard", "jobs");
    let matrix =
        shard["strategy"]["matrix"]["shard"].as_sequence().map(|values| values.iter().map(ruby_to_s).collect());
    assert!(
        matrix == Some(Some(shards)) && shard["strategy"]["fail-fast"] == Value::Bool(false),
        "CI must run every shard listed by make print-ci-macos-shards"
    );
    let env = required(&shard, "env", "macos-shard");
    assert!(env["CARGO_PROFILE"] == "ci", "only PR CI may select the fast ci Cargo profile");
    let mut keys: Vec<Option<&str>> =
        env.as_mapping().map_or(Vec::new(), |env| env.keys().map(Value::as_str).collect());
    keys.sort();
    // Every updater gate owns its HOME and install roots, so all lanes run as
    // the runner's own user and no second macOS user is created.
    let steps = required(&shard, "steps", "macos-shard");
    let creates_user = steps
        .as_sequence()
        .expect("macos-shard steps")
        .iter()
        .any(|step| step.get("run").and_then(ruby_to_s).unwrap_or_default().contains("dscl"));
    assert!(
        keys == [Some("CARGO_PROFILE")] && !creates_user,
        "CI shards must run every lane as the runner's user with only CARGO_PROFILE set"
    );
    let gate = required(&jobs, "macos", "jobs");
    let mut shards_env = Mapping::new();
    shards_env.insert("SHARDS".into(), "${{ needs.macos-shard.result }}".into());
    let steps: Option<Vec<(Option<&Value>, Option<&Value>)>> = gate
        .get("steps")
        .and_then(Value::as_sequence)
        .map(|steps| steps.iter().map(|step| (step.get("env"), step.get("run"))).collect());
    assert!(
        required(&gate, "name", "macos") == "macOS build and regression gates"
            && gate["needs"] == "macos-shard"
            && gate["if"] == "always()"
            && steps
                == Some(vec![(Some(&Value::Mapping(shards_env)), Some(&Value::from(r#"test "$SHARDS" = success"#)))]),
        "the required macOS check must fail unless every shard passes"
    );
}

pub fn ci_workflow_runs_gates_in_nix_shells() {
    assert_lines(
        &read(CI),
        &[
            "  contents: read",
            "    runs-on: ubuntu-24.04",
            "    runs-on: macos-15",
            "        uses: cachix/install-nix-action@13d8dd58da0234aa297dedd986986ccb8e7f3e24 # v31.11.1",
            "        run: nix flake check --print-build-logs",
            "        run: nix develop .#ci --command make -j1 test-portable",
            "      - name: Run macOS regression gates with the system Apple SDK",
            "        run: sudo sh -c 'cat /etc/zshrc >/etc/zshrc.hamn-ci && mv /etc/zshrc.hamn-ci /etc/zshrc'",
            "        run: nix develop .#ci --command make -j1 ci-macos-shard-${{ matrix.shard }}",
        ],
        "Nix CI workflow is incomplete",
    );
}

/// What one run of the macOS shards' network wait did.
struct NetworkWait {
    status: ExitStatus,
    stdout: String,
    /// The arguments of each `curl` call, in order.
    probes: Vec<String>,
    /// The argument of each `sleep` call, in order.
    sleeps: Vec<String>,
}

/// Runs `script` as a runner runs a `shell: bash` step, with a `curl` that
/// cannot resolve the host in its first `unreachable_probes` calls and a
/// `sleep` that returns at once.
fn network_wait(script: &str, unreachable_probes: u32) -> NetworkWait {
    let work = TempDir::new("hamn-network-wait.");
    let (bin, probes, sleeps) = (work.path().join("bin"), work.path().join("probes"), work.path().join("sleeps"));
    fs::create_dir(&bin).unwrap();
    fs::write(&probes, "").unwrap();
    fs::write(&sleeps, "").unwrap();
    write_executable(
        &bin.join("curl"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >>'{probes}'\n[ \"$(($(wc -l <'{probes}')))\" -gt {unreachable_probes} ] && exit 0\necho 'curl: (6) Could not resolve host: github.com' >&2\nexit 6\n",
            probes = probes.display()
        ),
    );
    write_executable(&bin.join("sleep"), &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >>'{}'\n", sleeps.display()));
    let step = work.path().join("step.sh");
    fs::write(&step, script).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH"));
    let output = output_within(
        Command::new("/bin/bash").args(["--noprofile", "--norc", "-e", "-o", "pipefail"]).arg(&step).env("PATH", path),
        Duration::from_secs(60),
    );
    let calls = |path: &Path| lines(&fs::read_to_string(path).unwrap()).into_iter().map(str::to_owned).collect();
    NetworkWait {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        probes: calls(&probes),
        sleeps: calls(&sleeps),
    }
}

/// The `--max-time` seconds of a `curl` call that probes https://github.com.
fn probe_limit(arguments: &str) -> u64 {
    let arguments: Vec<&str> = arguments.split(' ').collect();
    assert_eq!(
        arguments.last(),
        Some(&"https://github.com"),
        "the wait must probe the host that checkout fetches from"
    );
    let limit = arguments.iter().position(|argument| *argument == "--max-time").and_then(|at| arguments.get(at + 1));
    limit.and_then(|limit| limit.parse().ok()).expect("every probe must be bounded by --max-time SECONDS")
}

/// A macOS shard waits until it reaches github.com and only then checks
/// out: actions/checkout gives up at most 40 seconds after its first fetch,
/// and in run 38319967795 (attempt 2) a runner could not resolve github.com
/// for that long. The wait is bounded, so a runner that stays cut off fails
/// its job and leaves most of the job's 30 minutes to the gates.
pub fn ci_macos_shards_wait_for_github_before_checkout() {
    let workflow: Value = serde_yaml::from_str(&read(CI)).unwrap_or_else(|error| panic!("{CI}: {error}"));
    let steps = workflow["jobs"]["macos-shard"]["steps"].as_sequence().expect("macos-shard steps");
    assert!(
        steps.len() > 1
            && steps[0]["name"] == "Wait for the runner to reach github.com"
            && steps[0]["shell"] == "bash"
            && steps[1]["uses"].as_str().is_some_and(|uses| uses.starts_with("actions/checkout@")),
        "a macOS shard must wait for github.com immediately before it checks out"
    );
    let script = steps[0]["run"].as_str().expect("the network wait runs a script");

    let reachable = network_wait(script, 0);
    assert!(
        reachable.status.success() && reachable.probes.len() == 1 && reachable.sleeps.is_empty(),
        "a runner that reaches github.com must check out at once: {:?}, {} probes, {} sleeps",
        reachable.status,
        reachable.probes.len(),
        reachable.sleeps.len()
    );
    // The failed run: three fetches in a row could not resolve the host.
    let recovered = network_wait(script, 3);
    assert!(
        recovered.status.success() && recovered.probes.len() == 4 && recovered.sleeps.len() == 3,
        "the wait must end at the first probe that reaches github.com: {:?}, {} probes, {} sleeps",
        recovered.status,
        recovered.probes.len(),
        recovered.sleeps.len()
    );
    let unreachable = network_wait(script, u32::MAX);
    assert!(
        unreachable.status.code() == Some(1) && unreachable.stdout.contains("::error::"),
        "a runner that never reaches github.com must fail the step with an error annotation: {:?}: {}",
        unreachable.status,
        unreachable.stdout
    );
    let probing: u64 = unreachable.probes.iter().map(|probe| probe_limit(probe)).sum();
    let sleeping: u64 = unreachable.sleeps.iter().map(|sleep| sleep.parse::<u64>().expect("sleep SECONDS")).sum();
    // Three times the checkout's own 40 seconds when every probe fails at
    // once, and a third of the job's 30 minutes when every probe times out.
    assert!(
        sleeping >= 120 && probing + sleeping <= 600,
        "the wait must last between 120 and 600 seconds: {sleeping} to {} seconds",
        probing + sleeping
    );
}

/// The Darwin shells, not per-workflow scripts, own the Apple SDK boundary.
pub fn workflows_leave_sdk_selection_to_the_flake() {
    for file in workflow_files() {
        assert!(
            !any_line_matches(&read(&file), r#"source scripts/ci/|SDKROOT="\$HAMN_SYSTEM_SDKROOT"|system_sdk="#),
            "workflows must use the flake shell SDK selection instead of their own: {file}"
        );
    }
}

pub fn release_please_workflow_is_complete() {
    assert_lines(
        &read(RELEASE_PLEASE),
        &[
            "    workflows: [Release]",
            "    types: [completed]",
            "    if: (github.event_name != 'workflow_run' || github.event.workflow_run.conclusion == 'success') && (github.event_name != 'push' || !contains(github.event.head_commit.message, '[skip release]'))",
            "          ref: main",
            "        run: cargo run --locked -p hamn-dev -- release pr-ready",
            "        if: steps.publication.outputs.ready == 'true'",
            "  contents: read",
            "        uses: googleapis/release-please-action@45996ed1f6d02564a971a2fa1b5860e934307cf7 # v5.0.0",
            "          token: ${{ github.token }}",
            "          skip-github-release: true",
            // The automatic token raises no pull_request event, so the release
            // PR's required checks exist only because this step dispatches them.
            // Reading runs is enough: approving one is a maintainer's decision.
            "      actions: read",
            "        run: cargo run --locked -p hamn-dev -- release report-pr",
            // Without this the step is skipped exactly when it has something
            // to report: after Release Please could not write the release PR.
            "        if: ${{ !cancelled() && steps.publication.outputs.ready == 'true' }}",
        ],
        "Release Please workflow is incomplete",
    );
}

/// No workflow may depend on a stored credential: only the automatic
/// per-run token, which cannot expire and is scoped to its own job.
pub fn workflows_use_only_the_automatic_token() {
    let secret = Regex::new(r"secrets\.([A-Za-z0-9_]+)").unwrap();
    for file in workflow_files() {
        let text = read(&file);
        for line in lines(&text) {
            for name in secret.captures_iter(line).map(|captures| captures[1].to_owned()) {
                assert_eq!(name, "GITHUB_TOKEN", "workflow depends on a stored secret: {file}: {line}");
            }
        }
    }
}

/// `grep -c`: lines holding `text` across every workflow file.
fn workflow_lines_containing(text: &str) -> usize {
    workflow_files().iter().map(|file| lines(&read(file)).iter().filter(|line| line.contains(text)).count()).sum()
}

pub fn checkouts_do_not_persist_credentials() {
    let checkouts = workflow_lines_containing("uses: actions/checkout@");
    assert!(checkouts > 0, "workflows do not check out source");
    assert_eq!(
        workflow_lines_containing("          persist-credentials: false"),
        checkouts,
        "every checkout must disable credential persistence"
    );
}

pub fn release_workflow_is_automated_and_hosted() {
    let release = read(RELEASE);
    assert_lines(
        &release,
        &[
            "    branches: [main]",
            "      - '.release-please-manifest.json'",
            "  workflow_dispatch:",
            "  contents: read",
            "    name: Resolve release version",
            r#"            "$HAMN_DEV" release resolve-release "$PREVIOUS_REF""#,
            r#"            "$HAMN_DEV" release recover-release"#,
        ],
        "automated release trigger is incomplete",
    );
    assert!(
        !release.contains("CARGO_PROFILE"),
        "release candidates must be built and gated with the release Cargo profile"
    );
    assert!(
        !release.contains("commit.verification.verified"),
        "release workflow still requires GitHub Verified commits"
    );
    assert!(
        !any_line_matches(&release, r"^[[:space:]]+tags:|rc_run_id:|inputs\.rc_"),
        "release workflow exposes an arbitrary tag or cross-run input"
    );
    assert!(
        !any_line_matches(&release, "runs-on:.*self-hosted|environment: hamn-validation"),
        "automatic releases must use GitHub-hosted runners only"
    );
}

pub fn guest_image_job_is_complete() {
    let release = read(RELEASE);
    assert_section_lines(
        &section(&release, "  guest-image:", Some("  candidate:")),
        &[
            "    runs-on: ubuntu-24.04-arm",
            "    timeout-minutes: 120",
            "      attestations: write",
            "      contents: read",
            "      id-token: write",
            "            dhcpcd-base ipxe-qemu jq libguestfs-tools linux-image-virtual",
            "          sudo apt-get purge --yes passt",
            "          if command -v passt >/dev/null 2>&1; then",
            r#"            printf 'dhcpcd-base\n' | sudo tee -a "$guestfs_packages" >/dev/null"#,
            r#"          printf 'nameserver 169.254.2.3\n' > "$resolver_overlay/etc/resolv.conf""#,
            r#"            "$guestfs_supermin/zz-hamn-resolver.tar.gz""#,
            "          sudo chmod a+r /boot/vmlinuz-*",
            r"          LIBGUESTFS_BACKEND_SETTINGS=force_tcg \",
            r#"            libguestfs-test-tool 2>&1 | tee "$guestfs_test_log""#,
            r#"          grep -Fq 'nameserver 169.254.2.3' "$guestfs_test_log""#,
            r"          LIBGUESTFS_DEBUG=1 \",
            r"          LIBGUESTFS_TRACE=1 \",
            r"          sudo -u nobody /usr/bin/env -i \",
            "          builder=$(mktemp -d /tmp/hamn-guest-builder.XXXXXX)",
            r"          git clone --quiet --no-hardlinks --no-checkout \",
            r#"          GIT_CONFIG_VALUE_0="$builder/source" \"#,
            r#"          HAMN_GUEST_BASE_IMAGE="$builder/input/base.img" \"#,
            r#"          XDG_RUNTIME_DIR="$builder/runtime" \"#,
            r"          GIT_CONFIG_KEY_0=safe.directory \",
            r#"            /bin/bash "$builder/source/guest/image/build-ubuntu-24.04-arm64.sh""#,
            r#"          sudo install -o "$(id -u)" -g "$(id -g)" -m 0644 \"#,
            r#"            "$guest_output" "$RUNNER_TEMP/hamn-guest.img""#,
            r#"            "$guest_output.sha256" "$RUNNER_TEMP/hamn-guest.img.sha256""#,
            "      - name: Attest completed guest image",
        ],
        "guest image job is incomplete",
    );
}

pub fn candidate_job_runs_hosted_validation() {
    let release = read(RELEASE);
    assert_section_lines(
        &section(&release, "  candidate:", Some("  publish:")),
        &[
            "    runs-on: macos-15",
            "      artifact-metadata: write",
            "      attestations: write",
            "      contents: read",
            "      id-token: write",
            "          nix develop .#ci --command make -j1 test-local-macos",
            r"          nix develop .#ci --command make release-candidate \",
            r"          nix develop .#ci --command make release-hosted-validation \",
            "      - name: Attest exact candidate artifact provenance",
            "      - name: Attest hosted validation evidence",
        ],
        "candidate hosted-validation job is incomplete",
    );
}

pub fn publish_job_promotes_a_keyless_immutable_release() {
    let release = read(RELEASE);
    let publish = section(&release, "  publish:", None);
    assert_section_lines(
        &publish,
        &[
            "    name: Publish immutable keyless release",
            "    needs: [prepare, candidate]",
            "    environment: hamn-promotion",
            "      attestations: write",
            "      contents: write",
            "      id-token: write",
            "      - name: Verify keyless build provenance",
            "        run: cargo build --locked -p hamn-dev",
            "          HAMN_DEV: ${{ github.workspace }}/target/debug/hamn-dev",
            r#"          "$HAMN_DEV" release publish \"#,
            "      - name: Attest immutable update manifest",
            "      - name: Create and verify draft release",
            r#"            --draft --target "$GITHUB_SHA" --title "Hamn $STABLE_TAG" \"#,
            "      - name: Publish immutable release and verify assets",
            r#"          gh release edit "$STABLE_TAG" --draft=false --latest"#,
            r"            --json tagName,targetCommitish,isDraft,isPrerelease,isImmutable \",
            r#"          gh release verify "$STABLE_TAG""#,
        ],
        "keyless immutable promotion is incomplete",
    );
    let has = |text: &str| publish.iter().any(|line| line.contains(text));
    assert!(
        has(r#"gh attestation verify "$candidate/$name""#),
        "keyless promotion does not verify each candidate attestation"
    );
    assert!(has("--deny-self-hosted-runners"), "keyless promotion accepts provenance from self-hosted runners");
    assert!(has(r#""$publish/hamn-update-manifest-v3.json""#), "promotion omits the v3 manifest asset");
    assert!(!has("hamn-update-manifest.json"), "promotion still publishes the removed schema v2 manifest");
}

pub fn release_workflow_needs_no_manual_tag() {
    assert_absent(
        r"git[[:space:]]+tag|git[[:space:]]+push.*refs/tags|--verify-tag",
        &[RELEASE],
        "release workflow still depends on a manually prepared tag",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_run_from_each_start_line_to_the_next_stop_line() {
        let text = "a\n  job:\nx\n  next:\ny\n  job:\nz\n";
        assert_eq!(section(text, "  job:", Some("  next:")), ["  job:", "x", "  job:", "z"]);
        assert_eq!(section(text, "  next:", None), ["  next:", "y", "  job:", "z"]);
        assert!(section(text, "  missing:", None).is_empty());
    }

    #[test]
    fn probe_limits_are_the_max_time_seconds_of_a_github_probe() {
        assert_eq!(probe_limit("--silent --head --max-time 10 https://github.com"), 10);
    }

    #[test]
    #[should_panic(expected = "bounded by --max-time")]
    fn probes_without_a_time_limit_are_rejected() {
        probe_limit("--silent --head https://github.com");
    }

    #[test]
    #[should_panic(expected = "the host that checkout fetches from")]
    fn probes_of_another_host_are_rejected() {
        probe_limit("--head --max-time 10 https://example.com");
    }

    #[test]
    fn scalars_convert_like_ruby_to_s() {
        let values: Vec<Value> = serde_yaml::from_str("[1, '2', true, null, [3]]").unwrap();
        let converted: Vec<Option<String>> = values.iter().map(ruby_to_s).collect();
        assert_eq!(converted, [Some("1".into()), Some("2".into()), Some("true".into()), Some(String::new()), None]);
    }
}
