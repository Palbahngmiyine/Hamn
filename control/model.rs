use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Default, Parser, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[command(name = "hamn", version = crate::core::version(), about = "VM, Docker and Kubernetes console",
    long_about = "VM, Docker and Kubernetes console. Run without arguments for TUI, or use --headless <operation> for JSON.\nvm delete preserves the VM disk and Docker data. system uninstall permanently removes all Hamn data.",
    after_help = "Operations and their arguments: hamn --headless capabilities")]
pub struct Request {
    #[arg(long)]
    pub uid: Option<String>,
    #[arg(long)]
    pub container: Option<String>,
    #[arg(long)]
    pub previous: bool,
    #[arg(long)]
    pub headless: bool,
    #[arg(num_args = 1..=4)]
    pub words: Vec<String>,
    #[arg(long)]
    pub profile: Option<String>,
    #[arg(long)]
    pub context: Option<String>,
    #[arg(
        long,
        help = "Docker CLI configuration directory for an explicit Docker context"
    )]
    pub docker_config: Option<String>,
    #[arg(long)]
    pub namespace: Option<String>,
    #[arg(long)]
    pub kubeconfig: Option<String>,
    #[arg(long, help = "Confirm mutations, including system upgrade")]
    pub yes: bool,
    #[arg(long)]
    pub cpu: Option<u32>,
    #[arg(long)]
    pub memory: Option<u32>,
    #[arg(long)]
    pub disk: Option<u32>,
    #[arg(
        long,
        value_name = "BOOL",
        action = clap::ArgAction::Set,
        help = "Apple Linux Rosetta translation for vm create and vm configure: true or false"
    )]
    pub rosetta: Option<bool>,
    #[arg(long)]
    pub replicas: Option<u32>,
    #[arg(long)]
    pub path: Option<String>,
    // The two vm apply arguments are left out of the worker request unless
    // they are set, so that every other request keeps its form.
    #[arg(long, help = "Profile definition file for vm apply")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[arg(
        long,
        help = "Report what vm apply would change without writing; no --yes required"
    )]
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
    #[arg(
        long,
        help = "Release manifest URL for system upgrade (defaults to latest stable)"
    )]
    pub manifest: Option<String>,
    #[arg(long, help = "Read release metadata only for system upgrade")]
    pub check: bool,
    #[arg(
        long,
        help = "Reinstall the same host release for system upgrade; never downgrade"
    )]
    pub force: bool,
    #[arg(long)]
    pub follow: bool,
    #[arg(long)]
    pub watch: bool,
    #[arg(long)]
    pub all_namespaces: bool,
    #[arg(long, default_value_t = 200)]
    pub tail: u32,
    #[arg(long, default_value_t = 600)]
    pub timeout: u64,
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Failure {
    pub code: String,
    pub message: String,
}

impl Failure {
    pub fn new(code: &str, message: impl ToString) -> Self {
        Self {
            code: code.into(),
            message: message.to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Failure>;

// Both frontends and capabilities use this registry.
pub const OPERATIONS: &[(&str, bool)] = &[
    ("vm list", false),
    ("vm status", false),
    ("vm create", true),
    ("vm configure", true),
    ("vm apply", true),
    ("vm start", true),
    ("vm stop", true),
    ("vm delete", true),
    ("vm diagnostics", true),
    ("vm env", false),
    ("system upgrade", true),
    ("system uninstall", true),
    ("docker containers list", false),
    ("docker containers inspect", false),
    ("docker containers logs", false),
    ("docker containers stats", false),
    ("docker containers start", true),
    ("docker containers stop", true),
    ("docker containers restart", true),
    ("docker containers delete", true),
    ("docker images list", false),
    ("docker volumes list", false),
    ("docker networks list", false),
    ("k8s contexts list", false),
    ("k8s namespaces list", false),
    ("k8s namespaces inspect", false),
    ("k8s pods list", false),
    ("k8s pods inspect", false),
    ("k8s pods logs", false),
    ("k8s pods delete", true),
    ("k8s deployments list", false),
    ("k8s deployments inspect", false),
    ("k8s deployments scale", true),
    ("k8s deployments restart", true),
    ("k8s statefulsets list", false),
    ("k8s statefulsets inspect", false),
    ("k8s statefulsets scale", true),
    ("k8s statefulsets restart", true),
    ("k8s daemonsets list", false),
    ("k8s daemonsets inspect", false),
    ("k8s daemonsets restart", true),
    ("k8s services list", false),
    ("k8s services inspect", false),
    ("k8s nodes list", false),
    ("k8s nodes inspect", false),
    ("k8s events list", false),
    ("k8s events inspect", false),
    ("k8s jobs list", false),
    ("k8s jobs inspect", false),
    ("k8s cronjobs list", false),
    ("k8s cronjobs inspect", false),
    ("k8s ingresses list", false),
    ("k8s ingresses inspect", false),
    ("k8s pvcs list", false),
    ("k8s pvcs inspect", false),
];

impl Request {
    pub fn normalize(&mut self) -> Result<()> {
        if self.words.len() == 4 {
            if self.name.is_some() {
                return Err(Failure::new("invalidRequest", "name specified twice"));
            }
            self.name = self.words.pop();
        }
        Ok(())
    }
    pub fn operation(&self) -> String {
        self.words.join(" ")
    }
    pub fn mutates(&self) -> bool {
        if self.check
            && matches!(
                self.operation().as_str(),
                "system upgrade"
            )
        {
            return false;
        }
        if self.dry_run && self.operation() == "vm apply" {
            return false;
        }
        OPERATIONS
            .iter()
            .any(|(op, mutation)| *op == self.operation() && *mutation)
    }
    pub fn impact(&self) -> String {
        match self.operation().as_str() {
            "vm create" => "Create a profile and its VM configuration.".into(),
            "vm configure" => "Change the stopped VM's CPU, memory, disk or Rosetta settings.".into(),
            "vm apply" => "Create the profile or replace its stopped VM's configuration with the definition file; settings the file omits return to their defaults. The VM is not started or stopped.".into(),
            "vm start" => "Start the VM.".into(),
            "vm stop" => "Stop the VM and interrupt its running containers.".into(),
            "vm delete" => "Stop and remove the profile from active listings. Its disk and Docker data are preserved.".into(),
            "vm diagnostics" => "Write a redacted diagnostic archive to the selected path.".into(),
            "system upgrade" => "Download and publish a verified Hamn release and managed guest image.".into(),
            "system uninstall" => "Permanently remove ALL Hamn profiles, VM disks, Docker data and the managed installation.".into(),
            "docker containers delete" => "Delete the selected container. Its named and anonymous volumes are preserved.".into(),
            "docker containers start" => "Start the selected container.".into(),
            "docker containers stop" => "Stop the selected container and interrupt its workload.".into(),
            "docker containers restart" => "Restart the selected container and interrupt its workload.".into(),
            "k8s pods delete" => "Delete the selected Pod. Its controller may create a replacement.".into(),
            _ if self.words.last().is_some_and(|v| v == "scale") => format!("Set the selected workload to {} replicas; Pods may be created or terminated.", self.replicas.unwrap_or(0)),
            _ if self.words.last().is_some_and(|v| v == "restart") => "Request a rolling restart of the selected workload's Pods.".into(),
            _ => "Read the selected resource without changing it.".into(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        let invalid = |message| Failure::new("invalidRequest", message);
        if !OPERATIONS.iter().any(|(op, _)| *op == self.operation()) {
            // `vm status work`: a profile-taking VM operation followed by a word.
            let positional_profile = self.words.len() > 2
                && self.words[0] == "vm"
                && self.words[1] != "list"
                && OPERATIONS
                    .iter()
                    .any(|(op, _)| *op == self.words[..2].join(" "));
            return Err(invalid(if positional_profile && self.words[1] == "apply" {
                "unknown operation; pass the profile definition with --file <path> and the profile with --profile <name>, not a positional word"
            } else if positional_profile {
                "unknown operation; select the VM profile with --profile <name>, not a positional word"
            } else {
                "unknown operation; use --headless capabilities"
            }));
        }
        if self.timeout == 0 || self.timeout > 3600 || self.tail > 10000 {
            return Err(invalid("timeout must be 1..3600 seconds and tail <= 10000"));
        }
        if self.mutates() && !self.yes {
            return Err(invalid("mutation requires --yes"));
        }
        if self.words.first().is_some_and(|word| word == "vm")
            && self.operation() != "vm list"
            && self.profile.is_none()
        {
            return Err(invalid(
                "an explicit --profile is required; list profiles with: hamn --headless vm list",
            ));
        }
        if self.words.first().is_some_and(|word| word == "docker")
            && (self.profile.is_some() == self.context.is_some())
        {
            return Err(invalid(if self.profile.is_some() {
                "Docker requires exactly one explicit --profile or --context"
            } else {
                "Docker requires exactly one explicit --profile or --context; list profiles with: hamn --headless vm list"
            }));
        }
        if self.docker_config.is_some()
            && (!self.words.first().is_some_and(|word| word == "docker") || self.context.is_none())
        {
            return Err(invalid(
                "--docker-config requires a Docker operation with --context",
            ));
        }
        if self
            .docker_config
            .as_ref()
            .is_some_and(|path| path.is_empty() || path.contains('\0'))
        {
            return Err(invalid("invalid Docker configuration directory"));
        }
        if self.words.first().is_some_and(|word| word == "docker")
            && self.context.as_ref().is_some_and(|context| {
                context.is_empty()
                    || context.chars().count() > 253
                    || context.chars().any(char::is_control)
            })
        {
            return Err(invalid("invalid context name"));
        }
        if (self.check || self.force)
            && !matches!(
                self.operation().as_str(),
                "system upgrade"
            )
        {
            return Err(invalid(
                "--check and --force are only supported for system upgrade",
            ));
        }
        if self.check && self.force {
            return Err(invalid("--check conflicts with --force"));
        }
        if self.rosetta.is_some()
            && !matches!(self.operation().as_str(), "vm create" | "vm configure")
        {
            return Err(invalid(
                "--rosetta is only supported for vm create and vm configure",
            ));
        }
        let apply = self.operation() == "vm apply";
        if !apply && (self.file.is_some() || self.dry_run) {
            return Err(invalid(
                "--file and --dry-run are only supported for vm apply",
            ));
        }
        if apply {
            match self.file.as_deref() {
                None | Some("") => return Err(invalid("vm apply requires --file <path>")),
                Some("-") => {
                    return Err(invalid(
                        "vm apply reads a regular file; standard input (-) is not supported",
                    ));
                }
                Some(path) if path.contains('\0') => {
                    return Err(invalid("invalid profile definition path"));
                }
                Some(_) => {}
            }
            if self.cpu.is_some() || self.memory.is_some() || self.disk.is_some() {
                return Err(invalid(
                    "--cpu, --memory and --disk cannot be combined with vm apply; the definition file holds every setting",
                ));
            }
            // A dry run is not a mutation, and is still one answer.
            if self.watch || self.follow {
                return Err(invalid("vm apply cannot be repeated or streamed"));
            }
        }
        if let Some(profile) = &self.profile {
            if profile.is_empty()
                || profile.len() >= 64
                || profile == "cache"
                || !profile
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            {
                return Err(invalid("invalid profile name"));
            }
        }
        if self.words.first().is_some_and(|v| v == "k8s")
            && self.words.get(1).is_none_or(|v| v != "contexts")
            && self.context.is_none()
        {
            return Err(invalid("an explicit --context is required"));
        }
        if self.mutates() && self.all_namespaces {
            return Err(invalid("mutations cannot target all namespaces"));
        }
        if self.all_namespaces && self.words.last().is_none_or(|v| v != "list") {
            return Err(invalid("--all-namespaces is only supported for lists"));
        }
        if self.namespace.as_ref().is_some_and(|v| {
            v.is_empty()
                || v.len() > 63
                || v.starts_with('-')
                || v.ends_with('-')
                || !v
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        }) {
            return Err(invalid("invalid Kubernetes namespace"));
        }
        if self.mutates() && (self.watch || self.follow) {
            return Err(invalid("mutations cannot be repeated or streamed"));
        }
        if self.follow && (self.watch || self.words.last().is_none_or(|word| word != "logs")) {
            return Err(invalid(
                "--follow is only supported for logs and cannot be combined with --watch",
            ));
        }
        if self.mutates()
            && self.words.first().is_some_and(|v| v == "k8s")
            && self.namespace.as_ref().is_none_or(|v| v.is_empty())
        {
            return Err(invalid("Kubernetes mutations require --namespace"));
        }
        if [self.cpu, self.memory, self.disk].contains(&Some(0)) {
            return Err(invalid("VM resource values must be positive"));
        }
        if self.name.as_ref().is_some_and(|n| {
            n.is_empty()
                || n.len() > 253
                || !n
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        }) {
            return Err(invalid("invalid resource name"));
        }
        if self.words.last().is_some_and(|v| {
            matches!(
                v.as_str(),
                "inspect" | "logs" | "stats" | "scale" | "restart" | "delete" | "stop" | "start"
            )
        }) && matches!(
            self.words.first().map(String::as_str),
            Some("docker" | "k8s")
        ) && self.name.as_ref().is_none_or(|n| n.is_empty())
        {
            return Err(invalid("--name is required"));
        }
        if self.words.last().is_some_and(|v| v == "scale") && self.replicas.is_none() {
            return Err(invalid("scale requires --replicas"));
        }
        Ok(())
    }
    pub fn target(&self) -> Value {
        json!({"profile":self.profile,"context":self.context,
            "namespace":self.namespace,"name":self.name,"dockerConfig":self.docker_config})
    }
}

pub fn envelope(request: &Request, id: &str, result: Result<Value>) -> Value {
    let (data, error) = match result {
        Ok(data) => (data, Value::Null),
        Err(error) => (Value::Null, json!(error)),
    };
    json!({"schemaVersion":1,"requestId":id,"ok":error.is_null(),
        "target":request.target(),"data":data,"error":error})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_implicit_mutations_and_path_escape() {
        let mut r =
            Request::try_parse_from(["hamn", "--headless", "vm", "start", "--profile", "test"])
                .unwrap();
        assert!(r.validate().is_err());
        r.yes = true;
        assert!(r.validate().is_ok());
        r.profile = Some("../escape".into());
        assert!(r.validate().is_err());
    }
    fn rejection(arguments: &[&str]) -> String {
        let mut request = Request::try_parse_from(arguments).unwrap();
        request.normalize().unwrap();
        request.validate().unwrap_err().message
    }
    #[test]
    fn a_positional_vm_profile_is_rejected_with_the_profile_flag() {
        for arguments in [
            &["hamn", "--headless", "vm", "status", "work"][..],
            &["hamn", "--headless", "vm", "start", "work", "--yes"],
            &["hamn", "--headless", "vm", "status", "work", "extra"],
        ] {
            let message = rejection(arguments);
            assert!(
                message.starts_with("unknown operation") && message.contains("--profile <name>"),
                "{message}"
            );
        }
        // Words that follow no profile-taking VM operation keep the registry hint.
        for arguments in [
            &["hamn", "--headless", "vm", "resume"][..],
            &["hamn", "--headless", "vm", "resume", "work"],
            &["hamn", "--headless", "vm", "list", "work"],
            &["hamn", "--headless", "docker", "containers", "prune"],
        ] {
            assert_eq!(
                rejection(arguments),
                "unknown operation; use --headless capabilities"
            );
        }
    }
    #[test]
    fn a_missing_target_names_the_profile_listing() {
        for arguments in [
            &["hamn", "--headless", "vm", "status"][..],
            &["hamn", "--headless", "vm", "start", "--yes"],
            &["hamn", "--headless", "docker", "containers", "list"],
        ] {
            let message = rejection(arguments);
            assert!(
                message.contains("--profile") && message.ends_with("hamn --headless vm list"),
                "{message}"
            );
        }
        // Naming both Docker targets is not a missing profile.
        let both = rejection(&[
            "hamn",
            "--headless",
            "docker",
            "containers",
            "list",
            "--profile",
            "work",
            "--context",
            "remote",
        ]);
        assert_eq!(
            both,
            "Docker requires exactly one explicit --profile or --context"
        );
    }
    #[test]
    fn container_deletion_impact_covers_anonymous_volumes() {
        let request = Request {
            words: vec!["docker".into(), "containers".into(), "delete".into()],
            ..Default::default()
        };
        assert_eq!(
            request.impact(),
            "Delete the selected container. Its named and anonymous volumes are preserved."
        );
    }
    #[test]
    fn help_names_the_capabilities_listing() {
        use clap::CommandFactory;
        let mut command = Request::command();
        for help in [command.render_help(), command.render_long_help()] {
            let help = help.to_string();
            assert!(help.contains("hamn --headless capabilities"), "{help}");
        }
    }
    #[test]
    fn rosetta_takes_a_boolean_and_only_configures_a_profile() {
        let parse = |operation: &str, value: &str| {
            Request::try_parse_from([
                "hamn",
                "--headless",
                "vm",
                operation,
                "--profile",
                "work",
                "--yes",
                "--rosetta",
                value,
            ])
        };
        for operation in ["create", "configure"] {
            assert_eq!(parse(operation, "true").unwrap().rosetta, Some(true));
            let off = parse(operation, "false").unwrap();
            assert!(off.rosetta == Some(false) && off.validate().is_ok());
            assert!(parse(operation, "maybe").is_err());
        }
        for operation in ["start", "stop", "status", "delete"] {
            let message = parse(operation, "true")
                .unwrap()
                .validate()
                .unwrap_err()
                .message;
            assert_eq!(
                message,
                "--rosetta is only supported for vm create and vm configure"
            );
        }
        let unset = Request::try_parse_from(["hamn", "--headless", "vm", "list"]).unwrap();
        assert_eq!(unset.rosetta, None);
    }
    #[test]
    fn vm_apply_takes_a_definition_file_and_a_dry_run_without_confirmation() {
        let parse = |arguments: &[&str]| {
            let mut request = Request::try_parse_from(
                [
                    &["hamn", "--headless", "vm", "apply", "--profile", "work"][..],
                    arguments,
                ]
                .concat(),
            )
            .unwrap();
            request.normalize().unwrap();
            request
        };
        let apply = parse(&["--file", "work.yaml", "--yes"]);
        assert!(apply.validate().is_ok() && apply.mutates());
        assert_eq!(apply.file.as_deref(), Some("work.yaml"));
        // A dry run changes nothing, so it is no mutation and needs no --yes.
        let dry_run = parse(&["--file", "work.yaml", "--dry-run"]);
        assert!(dry_run.dry_run && !dry_run.mutates() && dry_run.validate().is_ok());
        for (arguments, message) in [
            (&["--file", "work.yaml"][..], "mutation requires --yes"),
            (&["--yes"], "vm apply requires --file <path>"),
            (&["--dry-run"], "vm apply requires --file <path>"),
            (&["--file", "", "--yes"], "vm apply requires --file <path>"),
            (
                &["--file", "-", "--yes"],
                "vm apply reads a regular file; standard input (-) is not supported",
            ),
            (
                &["--file", "work.yaml", "--yes", "--cpu", "2"],
                "--cpu, --memory and --disk cannot be combined with vm apply; the definition file holds every setting",
            ),
            (
                &["--file", "work.yaml", "--yes", "--memory", "8"],
                "--cpu, --memory and --disk cannot be combined with vm apply; the definition file holds every setting",
            ),
            (
                &["--file", "work.yaml", "--dry-run", "--disk", "80"],
                "--cpu, --memory and --disk cannot be combined with vm apply; the definition file holds every setting",
            ),
            (
                &["--file", "work.yaml", "--yes", "--rosetta", "true"],
                "--rosetta is only supported for vm create and vm configure",
            ),
            (
                &["--file", "work.yaml", "--yes", "--watch"],
                "vm apply cannot be repeated or streamed",
            ),
            (
                &["--file", "work.yaml", "--dry-run", "--watch"],
                "vm apply cannot be repeated or streamed",
            ),
            (
                &["--file", "work.yaml", "--dry-run", "--follow"],
                "vm apply cannot be repeated or streamed",
            ),
        ] {
            assert_eq!(
                parse(arguments).validate().unwrap_err().message,
                message,
                "{arguments:?}"
            );
        }
        // The profile is named by --profile, as for every VM operation, and
        // the file by --file: neither is a positional word.
        assert_eq!(
            rejection(&["hamn", "--headless", "vm", "apply", "--file", "work.yaml", "--yes"]),
            "an explicit --profile is required; list profiles with: hamn --headless vm list"
        );
        let positional = rejection(&[
            "hamn", "--headless", "vm", "apply", "work.yaml", "--profile", "work", "--yes",
        ]);
        assert!(
            positional.starts_with("unknown operation")
                && positional.contains("--file <path>")
                && positional.contains("--profile <name>"),
            "{positional}"
        );
        // Both arguments belong to vm apply alone.
        for arguments in [
            &["hamn", "--headless", "vm", "status", "--profile", "work", "--file", "work.yaml"][..],
            &["hamn", "--headless", "vm", "start", "--profile", "work", "--yes", "--dry-run"],
            &["hamn", "--headless", "vm", "list", "--dry-run"],
        ] {
            assert_eq!(
                rejection(arguments),
                "--file and --dry-run are only supported for vm apply"
            );
        }
    }
    #[test]
    fn only_vm_apply_adds_its_arguments_to_the_worker_request() {
        let serialized = |arguments: &[&str]| {
            serde_json::to_value(Request::try_parse_from(arguments).unwrap()).unwrap()
        };
        let start = serialized(&["hamn", "--headless", "vm", "start", "--profile", "work", "--yes"]);
        assert!(start.get("file").is_none() && start.get("dry_run").is_none(), "{start}");
        let apply = serialized(&[
            "hamn", "--headless", "vm", "apply", "--profile", "work", "--file", "work.yaml",
            "--dry-run",
        ]);
        assert!(apply["file"] == "work.yaml" && apply["dry_run"] == true, "{apply}");
        // The worker reads the request back with every field it was given.
        let read: Request = serde_json::from_value(apply).unwrap();
        assert!(read.dry_run && read.file.as_deref() == Some("work.yaml"));
        assert!(read.validate().is_ok());
    }
    #[test]
    fn failures_never_publish_success_data() {
        let result = envelope(
            &Request::default(),
            "test",
            Err(Failure::new("conflict", "busy")),
        );
        assert_eq!(result["ok"], false);
        assert!(result["data"].is_null());
    }
}

/// Read-only help selection. Parse all other flags normally so option values
/// named "system" or "upgrade" cannot accidentally select operation help.
pub fn upgrade_help(args: &[std::ffi::OsString]) -> Option<&'static str> {
    if !args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return None;
    }
    let request =
        Request::try_parse_from(args.iter().filter(|arg| *arg != "--help" && *arg != "-h")).ok()?;
    if !matches!(
        request.operation().as_str(),
        "system upgrade"
    ) {
        return None;
    }
    Some(
        "Upgrade Hamn to the latest stable release.\n\nUsage: hamn --headless system upgrade [--check | --yes [--force]] [--manifest URL]\n\nOptions:\n  --check           Read metadata only; no installation or recovery\n  --force           Reinstall the same host release; never downgrade\n  --yes             Required for installation, not --check\n  --manifest URL    Select a release manifest instead of latest stable\n  --timeout SECONDS Operation deadline, 1..3600 (default: 600)\n  -h, --help        Show this help without downloading or installing\n\nProgress is written to stderr; stdout contains the JSON result.\nThe host archive and guest image are verified before installation.\nExisting VMs are not restarted; the selected image is for new profile disks.\n\nRecovery: retry the same command with all original options (including --manifest) to recover an interrupted transaction.\nFor incompatible older installers, see https://github.com/Palbahngmiyine/Hamn#install\n",
    )
}

#[cfg(test)]
mod upgrade_help_tests {
    use super::*;
    #[test]
    fn docker_context_limits_do_not_narrow_existing_kubernetes_contexts() {
        let parse = |domain: &str, context: &str| {
            Request::try_parse_from([
                "hamn",
                "--headless",
                domain,
                if domain == "docker" {
                    "containers"
                } else {
                    "pods"
                },
                "list",
                "--context",
                context,
            ])
            .unwrap()
        };
        assert!(parse("k8s", &"x".repeat(254)).validate().is_ok());
        assert!(parse("docker", &"가".repeat(253)).validate().is_ok());
        for value in [String::new(), "x".repeat(254), "line\nbreak".into()] {
            assert!(parse("docker", &value).validate().is_err());
        }
    }
    fn help(args: &[&str]) -> Option<&'static str> {
        upgrade_help(
            &args
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>(),
        )
    }
    #[test]
    fn upgrade_help_handles_option_order_without_mutation_confirmation() {
        for args in [
            vec!["hamn", "--headless", "system", "upgrade", "--help"],
            vec![
                "hamn",
                "system",
                "upgrade",
                "-h",
                "--manifest",
                "https://example.test/release",
            ],
        ] {
            let text = help(&args).unwrap();
            assert!(text.contains("--yes"));
            assert!(text.contains("stderr"));
            assert!(text.contains("Existing VMs are not restarted"));
        }
    }
    #[test]
    fn unrelated_or_invalid_arguments_keep_normal_parser_help() {
        for args in [
            vec!["hamn", "--help"],
            vec!["hamn", "system", "upgrade"],
            vec!["hamn", "--profile", "system", "upgrade", "--help"],
            vec!["hamn", "system", "upgrade", "--unknown", "--help"],
            vec!["hamn", "system", "update", "--help"],
            vec!["hamn", "system", "uninstall", "--help"],
        ] {
            assert!(help(&args).is_none());
        }
    }
}
