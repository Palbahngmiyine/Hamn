use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Default, Parser, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[command(name = "hamn", version = env!("HAMN_VERSION"), about = "VM, Docker and Kubernetes console")]
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
    #[arg(long)]
    pub namespace: Option<String>,
    #[arg(long)]
    pub kubeconfig: Option<String>,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub cpu: Option<u32>,
    #[arg(long)]
    pub memory: Option<u32>,
    #[arg(long)]
    pub disk: Option<u32>,
    #[arg(long)]
    pub replicas: Option<u32>,
    #[arg(long)]
    pub path: Option<String>,
    #[arg(long)]
    pub manifest: Option<String>,
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
    ("vm migrate", true),
    ("vm start", true),
    ("vm stop", true),
    ("vm delete", true),
    ("vm diagnostics", true),
    ("vm env", false),
    ("system update", true),
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
    ("k8s nodes list", false),
    ("k8s events list", false),
    ("k8s jobs list", false),
    ("k8s cronjobs list", false),
    ("k8s ingresses list", false),
    ("k8s pvcs list", false),
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
        OPERATIONS
            .iter()
            .any(|(op, mutation)| *op == self.operation() && *mutation)
    }
    pub fn validate(&self) -> Result<()> {
        let invalid = |message| Failure::new("invalidRequest", message);
        if !OPERATIONS.iter().any(|(op, _)| *op == self.operation()) {
            return Err(invalid("unknown operation; use --headless capabilities"));
        }
        if self.timeout == 0 || self.timeout > 3600 || self.tail > 10000 {
            return Err(invalid("timeout must be 1..3600 seconds and tail <= 10000"));
        }
        if self.mutates() && !self.yes {
            return Err(invalid("mutation requires --yes"));
        }
        if matches!(
            self.words.first().map(String::as_str),
            Some("vm" | "docker")
        ) && self.operation() != "vm list"
            && self.profile.is_none()
        {
            return Err(invalid("an explicit --profile is required"));
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
            "namespace":self.namespace,"name":self.name})
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
