use crate::{
    core,
    model::{Request, Result},
};
use tokio::sync::mpsc;

pub async fn prepare(profile: &str) -> Result<serde_json::Value> {
    core::call(&Request {
        words: vec!["vm".into(), "migrate".into()],
        profile: Some(profile.into()),
        yes: true,
        timeout: 600,
        tail: 200,
        ..Default::default()
    })
    .await
}

// Dropping a frontend cancels only its worker; the C ownership supervisors
// retain responsibility for reaping operations and releasing locks.
pub struct Startup(tokio::task::JoinHandle<()>);
impl Drop for Startup {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl Startup {
    pub fn new(sender: mpsc::Sender<String>) -> Self {
        Self(tokio::spawn(async move {
            let list = Request {
                words: vec!["vm".into(), "list".into()],
                timeout: 600,
                tail: 200,
                ..Default::default()
            };
            let profiles = match core::call(&list).await {
                Ok(profiles) => profiles,
                Err(error) => {
                    let _ = sender.send(error.message).await;
                    return;
                }
            };
            for profile in profiles.as_array().into_iter().flatten() {
                if profile["migration"] != "pending" || profile["state"] == "stopped" {
                    continue;
                }
                let Some(name) = profile["name"].as_str() else {
                    continue;
                };
                let _ = sender.send(format!("Retiring managed K3s: {name}")).await;
                let result =
                    tokio::time::timeout(std::time::Duration::from_secs(600), prepare(name)).await;
                let message = match result {
                    Ok(Ok(value)) if value["migration"] == "current" => {
                        format!("K3s retirement complete: {name}")
                    }
                    Ok(Ok(_)) => format!("K3s retirement deferred until VM start: {name}"),
                    Ok(Err(error)) => {
                        format!("K3s retirement incomplete ({name}): {}", error.message)
                    }
                    Err(_) => {
                        format!("K3s retirement timed out ({name}); next mutation resumes it")
                    }
                };
                if sender.send(message).await.is_err() {
                    return;
                }
            }
        }))
    }
}
