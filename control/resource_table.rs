use crate::tui_theme::{self as theme, Tone};
use crate::{preferences::Workspace, tui_state::State};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    widgets::{Block, Cell, Row, Table, TableState},
};
use serde_json::Value;
fn text(value: &Value) -> String {
    crate::tui_state::clean(
        value
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| {
                if value.is_null() {
                    String::new()
                } else {
                    value.to_string()
                }
            })
            .as_str(),
    )
}
/// Docker IDs are shown as the CLI shows them by default: 12 hexadecimal
/// digits without the digest algorithm. Rows keep the full ID, which
/// selection and actions use.
fn short_id(id: String) -> String {
    let id = id.strip_prefix("sha256:").map(String::from).unwrap_or(id);
    id.chars().take(12).collect()
}
pub fn draw(frame: &mut Frame, area: Rect, state: &State, block: Block<'static>) {
    let invocation = state.native.as_ref().unwrap();
    let columns: &[&str] = if invocation.workspace == Workspace::Kubernetes {
        kube_columns(invocation.resource.as_deref().unwrap_or(""))
    } else {
        match invocation.resource.as_deref() {
            Some("projects") => &["Name", "Status", "ConfigFiles"],
            Some("images") => &["Repository", "Tag", "ID", "Size"],
            Some("volumes") => &["Name", "Driver", "Scope"],
            Some("networks") => &["ID", "Name", "Driver", "Scope"],
            Some("containers") if crate::native::show_size(invocation) => {
                &["ID", "Names", "Image", "Status", "Ports", "Size"]
            }
            _ => &["ID", "Names", "Image", "Status", "Ports"],
        }
    };
    // Rows are one line tall: two borders and one header leave this viewport.
    // Build expensive diagnostic/age cells only for rows that can be rendered.
    // Keep selection in global filtered-row coordinates; translate for Table.
    let visible = usize::from(area.height.saturating_sub(3));
    let offset = state
        .selected
        .unwrap_or(0)
        .saturating_sub(visible.saturating_sub(1));
    let now = jiff::Timestamp::now();
    let resource = invocation.resource.as_deref().unwrap_or("");
    // Rows of a failed refresh are dimmed: their states may be old.
    let stale = state.showing_stale_rows();
    let cells: Vec<(Vec<String>, Tone)> = state
        .rows()
        .into_iter()
        .skip(offset)
        .take(visible)
        .map(|row| {
            if invocation.workspace == Workspace::Kubernetes {
                (kube_fields(row, resource, now), kube_tone(row, resource))
            } else {
                let fields = columns
                    .iter()
                    .map(|name| {
                        let value = text(if *name == "Status" && row[name].is_null() {
                            &row["State"]
                        } else {
                            &row[name]
                        });
                        if *name == "ID" { short_id(value) } else { value }
                    })
                    .collect();
                (fields, docker_tone(row, resource))
            }
        })
        .collect();
    // Columns share the width in proportion to what they show, as k9s
    // sizes them to their content.
    let widths: Vec<_> = columns
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let content = cells
                .iter()
                .map(|(fields, _)| fields.get(index).map_or(0, |f| f.chars().count()))
                .max()
                .unwrap_or(0);
            Constraint::Fill(content.max(name.len()).clamp(4, 60) as u16)
        })
        .collect();
    let rows: Vec<Row> = cells
        .into_iter()
        .map(|(fields, tone)| {
            Row::new(fields.into_iter().map(Cell::from).collect::<Vec<_>>()).style(if stale {
                theme::dim()
            } else {
                theme::tone(tone)
            })
        })
        .collect();
    let table = Table::new(rows, widths)
        .header(
            Row::new(columns.iter().map(|name| name.to_uppercase()))
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(block)
        .row_highlight_style(theme::badge(invocation.workspace))
        .highlight_symbol("> ");
    frame.render_stateful_widget(
        table,
        area,
        &mut TableState::default().with_selected(state.selected.map(|index| index - offset)),
    );
}
/// How a Docker row reads: health and exit codes are in its status text.
fn docker_tone(row: &Value, resource: &str) -> Tone {
    let status = row["Status"].as_str().unwrap_or("");
    match resource {
        "containers" => match row["State"].as_str().unwrap_or("") {
            "running" if status.contains("(unhealthy)") => Tone::Failed,
            "running" if status.contains("(health: starting)") => Tone::Pending,
            "running" => Tone::Normal,
            "exited" if status.starts_with("Exited (0)") => Tone::Done,
            "exited" | "dead" => Tone::Failed,
            "created" | "paused" | "restarting" | "removing" => Tone::Pending,
            _ => Tone::Normal,
        },
        "projects" if status.contains("restarting") => Tone::Pending,
        "projects" if !status.contains("running") => Tone::Done,
        _ => Tone::Normal,
    }
}
/// How a Kubernetes row reads, from the same fields its columns show.
fn kube_tone(row: &Value, resource: &str) -> Tone {
    match resource {
        "pods" | "pod" | "po" => {
            let statuses = row["status"]["containerStatuses"].as_array();
            let all_ready = statuses.is_some_and(|s| s.iter().all(|s| s["ready"] == true));
            match pod_reason(row).as_str() {
                "Running" if all_ready => Tone::Normal,
                "Running" | "Pending" | "ContainerCreating" | "PodInitializing" | "Terminating" => {
                    Tone::Pending
                }
                "Succeeded" | "Completed" => Tone::Done,
                reason
                    if reason.starts_with("Init:")
                        && !reason.contains("Err")
                        && !reason.contains("BackOff") =>
                {
                    Tone::Pending
                }
                _ => Tone::Failed,
            }
        }
        "events" | "event" | "ev" if row["type"] == "Warning" => Tone::Pending,
        "nodes" | "node" | "no" => {
            let fields = kube_fields(row, resource, jiff::Timestamp::UNIX_EPOCH);
            if fields[2] != "True" {
                Tone::Failed
            } else if !fields[3].is_empty() {
                Tone::Pending
            } else {
                Tone::Normal
            }
        }
        "deployments" | "deployment" | "deploy" | "statefulsets" | "statefulset" | "sts"
        | "daemonsets" | "daemonset" | "ds" => {
            let fields = kube_fields(row, resource, jiff::Timestamp::UNIX_EPOCH);
            match fields[2].split_once('/') {
                Some((ready, desired)) if ready != desired => Tone::Pending,
                _ => Tone::Normal,
            }
        }
        _ => match row["status"]["phase"].as_str().unwrap_or("") {
            "Pending" | "Terminating" => Tone::Pending,
            "Failed" | "Lost" | "Unknown" => Tone::Failed,
            "Succeeded" => Tone::Done,
            _ => Tone::Normal,
        },
    }
}
fn kube_columns(resource: &str) -> &'static [&'static str] {
    match resource {
        "pods" | "pod" | "po" => &["Name", "Namespace", "Ready", "Restarts", "Reason", "Age"],
        "events" | "event" | "ev" => &["Name", "Namespace", "Type", "Reason", "Message"],
        "nodes" | "node" | "no" => &["Name", "Namespace", "Ready", "Conditions"],
        "deployments" | "deployment" | "deploy" | "statefulsets" | "statefulset" | "sts"
        | "daemonsets" | "daemonset" | "ds" => {
            &["Name", "Namespace", "Ready", "Updated", "Available"]
        }
        _ => &["Name", "Namespace", "Status"],
    }
}
fn count(value: &Value) -> u64 {
    value.as_u64().unwrap_or(0)
}
fn age(created: &Value, now: jiff::Timestamp) -> String {
    let Some(created) = created
        .as_str()
        .and_then(|s| s.parse::<jiff::Timestamp>().ok())
    else {
        return "?".into();
    };
    let seconds = now.as_second().saturating_sub(created.as_second()).max(0);
    if seconds >= 86400 {
        format!("{}d", seconds / 86400)
    } else if seconds >= 3600 {
        format!("{}h", seconds / 3600)
    } else if seconds >= 60 {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}
// Pod phase does not describe container readiness. Surface init/waiting and
// terminated reasons before phase, and retain abnormal container states.
fn pod_reason(row: &Value) -> String {
    if !row["metadata"]["deletionTimestamp"].is_null() {
        return "Terminating".into();
    }
    if let Some(reason) = row["status"]["reason"].as_str().filter(|s| !s.is_empty()) {
        return text(&Value::from(reason));
    }
    for field in ["initContainerStatuses", "containerStatuses"] {
        for status in row["status"][field].as_array().into_iter().flatten() {
            if let Some(reason) = status["state"]["waiting"]["reason"].as_str() {
                return text(&Value::from(reason));
            }
            if let Some(reason) = status["state"]["terminated"]["reason"].as_str() {
                if count(&status["state"]["terminated"]["exitCode"]) != 0 {
                    return text(&Value::from(reason));
                }
            }
        }
    }
    text(&row["status"]["phase"])
}
fn kube_fields(row: &Value, resource: &str, now: jiff::Timestamp) -> Vec<String> {
    let mut fields = vec![
        text(&row["metadata"]["name"]),
        text(&row["metadata"]["namespace"]),
    ];
    match resource {
        "pods" | "pod" | "po" => {
            let statuses: Vec<_> = row["status"]["containerStatuses"]
                .as_array()
                .into_iter()
                .flatten()
                .collect();
            let total = row["spec"]["containers"]
                .as_array()
                .map_or(statuses.len(), Vec::len);
            let ready = statuses.iter().filter(|s| s["ready"] == true).count();
            let restarts: u64 = ["containerStatuses", "initContainerStatuses"]
                .iter()
                .flat_map(|field| row["status"][field].as_array().into_iter().flatten())
                .map(|s| count(&s["restartCount"]))
                .fold(0, u64::saturating_add);
            fields.extend([
                format!("{ready}/{total}"),
                restarts.to_string(),
                pod_reason(row),
                age(&row["metadata"]["creationTimestamp"], now),
            ]);
        }
        "events" | "event" | "ev" => fields.extend([
            text(&row["type"]),
            text(&row["reason"]),
            text(if row["message"].is_null() {
                &row["note"]
            } else {
                &row["message"]
            }),
        ]),
        "nodes" | "node" | "no" => {
            let conditions: Vec<_> = row["status"]["conditions"]
                .as_array()
                .into_iter()
                .flatten()
                .collect();
            let ready = conditions
                .iter()
                .find(|c| c["type"] == "Ready")
                .map_or("Unknown".into(), |c| text(&c["status"]));
            let mut issues: Vec<_> = conditions
                .iter()
                .filter(|c| c["type"] != "Ready" && c["status"] == "True")
                .map(|c| text(&c["type"]))
                .collect();
            if row["spec"]["unschedulable"] == true {
                issues.push("SchedulingDisabled".into());
            }
            fields.extend([ready, issues.join(", ")]);
        }
        "deployments" | "deployment" | "deploy" | "statefulsets" | "statefulset" | "sts"
        | "daemonsets" | "daemonset" | "ds" => {
            let daemon = matches!(resource, "daemonsets" | "daemonset" | "ds");
            let desired = if daemon {
                count(&row["status"]["desiredNumberScheduled"])
            } else {
                row["spec"]["replicas"].as_u64().unwrap_or(1)
            };
            let ready = count(
                &row["status"][if daemon {
                    "numberReady"
                } else {
                    "readyReplicas"
                }],
            );
            fields.extend([
                format!("{ready}/{desired}"),
                count(
                    &row["status"][if daemon {
                        "updatedNumberScheduled"
                    } else {
                        "updatedReplicas"
                    }],
                )
                .to_string(),
                count(
                    &row["status"][if daemon {
                        "numberAvailable"
                    } else {
                        "availableReplicas"
                    }],
                )
                .to_string(),
            ]);
        }
        _ => fields.push(if !row["status"]["phase"].is_null() {
            text(&row["status"]["phase"])
        } else if !row["spec"]["clusterIP"].is_null() {
            format!(
                "{} {}",
                text(&row["spec"]["type"]),
                text(&row["spec"]["clusterIP"])
            )
        } else {
            String::new()
        }),
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn viewport_preserves_global_selection_at_the_end_of_large_filtered_lists() {
        let mut state = State::new(Default::default());
        state.workspace = Workspace::Kubernetes;
        state.native = Some(crate::native::parse("get pods", &state).unwrap());
        state.data = Value::Array((0..5000).map(|n| serde_json::json!({
            "metadata":{"name":format!("pod{n:04}"),"namespace":"work","uid":n.to_string()},
            "spec":{"containers":[{}]},"status":{"phase":"Running"}
        })).collect());
        state.selected = Some(4999);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 10)).unwrap();
        terminal
            .draw(|frame| draw(frame, frame.area(), &state, Block::bordered().title("pods")))
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("> pod4999"));
        assert!(!rendered.contains("pod0000"));
        state.filter = "pod000".into();
        state.selected = Some(9);
        terminal
            .draw(|frame| draw(frame, frame.area(), &state, Block::bordered().title("filtered")))
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("> pod0009"));
        assert!(!rendered.contains("pod4999"));
        assert_eq!(state.selected().unwrap()["metadata"]["name"], "pod0009");
    }
    #[test]
    fn pod_diagnostics_do_not_treat_running_phase_as_healthy() {
        let now = "2026-09-21T12:00:00Z".parse().unwrap();
        let mut pod = serde_json::json!({"metadata":{"name":"bad","namespace":"work","creationTimestamp":"2026-09-21T11:00:00Z"},"spec":{"containers":[{},{}]},"status":{"phase":"Running","containerStatuses":[{"ready":false,"restartCount":42,"state":{"waiting":{"reason":"CrashLoopBackOff"}}},{"ready":true}]}});
        assert_eq!(
            kube_fields(&pod, "pods", now),
            ["bad", "work", "1/2", "42", "CrashLoopBackOff", "1h"]
        );
        pod["status"]["containerStatuses"][0]["state"] =
            serde_json::json!({"waiting":{"reason":"ImagePullBackOff"}});
        assert_eq!(pod_reason(&pod), "ImagePullBackOff");
        pod["metadata"]["deletionTimestamp"] = "now".into();
        assert_eq!(pod_reason(&pod), "Terminating");
    }
    #[test]
    fn events_nodes_and_workloads_expose_actionable_fields() {
        let now = jiff::Timestamp::now();
        let event = serde_json::json!({"type":"Warning","reason":"FailedScheduling","message":"Insufficient memory"});
        assert_eq!(
            &kube_fields(&event, "events", now)[2..],
            ["Warning", "FailedScheduling", "Insufficient memory"]
        );
        let node = serde_json::json!({"spec":{"unschedulable":true},"status":{"conditions":[{"type":"Ready","status":"False"},{"type":"MemoryPressure","status":"True"}]}});
        assert_eq!(
            &kube_fields(&node, "nodes", now)[2..],
            ["False", "MemoryPressure, SchedulingDisabled"]
        );
        let deployment = serde_json::json!({"spec":{"replicas":3},"status":{"readyReplicas":1,"updatedReplicas":2,"availableReplicas":1}});
        assert_eq!(
            &kube_fields(&deployment, "deployments", now)[2..],
            ["1/3", "2", "1"]
        );
    }
    #[test]
    fn row_tones_follow_the_state_each_resource_reports() {
        let container = |state: &str, status: &str| serde_json::json!({"State": state, "Status": status});
        for (row, tone) in [
            (container("running", "Up 2 hours"), Tone::Normal),
            (container("running", "Up 2 hours (unhealthy)"), Tone::Failed),
            (container("running", "Up 3 seconds (health: starting)"), Tone::Pending),
            (container("exited", "Exited (0) 1 hour ago"), Tone::Done),
            (container("exited", "Exited (137) 1 hour ago"), Tone::Failed),
            (container("restarting", "Restarting (1) 2 seconds ago"), Tone::Pending),
            (container("dead", ""), Tone::Failed),
        ] {
            assert_eq!(docker_tone(&row, "containers"), tone, "{row}");
        }
        assert_eq!(docker_tone(&serde_json::json!({"Status": "exited(2)"}), "projects"), Tone::Done);
        assert_eq!(docker_tone(&serde_json::json!({"Status": "running(2)"}), "projects"), Tone::Normal);
        assert_eq!(docker_tone(&serde_json::json!({"Size": "1GB"}), "images"), Tone::Normal);
        let pod = |phase: &str, ready: bool, waiting: Option<&str>| {
            let mut status = serde_json::json!({"ready": ready});
            if let Some(reason) = waiting {
                status["state"] = serde_json::json!({"waiting": {"reason": reason}});
            }
            serde_json::json!({"spec": {"containers": [{}]}, "status": {"phase": phase, "containerStatuses": [status]}})
        };
        for (row, tone) in [
            (pod("Running", true, None), Tone::Normal),
            (pod("Running", false, None), Tone::Pending),
            (pod("Pending", false, Some("ContainerCreating")), Tone::Pending),
            (pod("Running", false, Some("CrashLoopBackOff")), Tone::Failed),
            (pod("Pending", false, Some("ImagePullBackOff")), Tone::Failed),
            (pod("Succeeded", false, None), Tone::Done),
            (pod("Failed", false, None), Tone::Failed),
        ] {
            assert_eq!(kube_tone(&row, "pods"), tone, "{row}");
        }
        let deployment = |ready: u64| serde_json::json!({"spec": {"replicas": 3}, "status": {"readyReplicas": ready}});
        assert_eq!(kube_tone(&deployment(3), "deployments"), Tone::Normal);
        assert_eq!(kube_tone(&deployment(1), "deployments"), Tone::Pending);
        let node = |ready: &str, pressure: &str| serde_json::json!({"status": {"conditions": [
            {"type": "Ready", "status": ready}, {"type": "DiskPressure", "status": pressure}]}});
        assert_eq!(kube_tone(&node("True", "False"), "nodes"), Tone::Normal);
        assert_eq!(kube_tone(&node("True", "True"), "nodes"), Tone::Pending);
        assert_eq!(kube_tone(&node("False", "False"), "nodes"), Tone::Failed);
        assert_eq!(kube_tone(&serde_json::json!({"type": "Warning"}), "events"), Tone::Pending);
        assert_eq!(kube_tone(&serde_json::json!({"status": {"phase": "Bound"}}), "pvcs"), Tone::Normal);
        assert_eq!(kube_tone(&serde_json::json!({"status": {"phase": "Lost"}}), "pvcs"), Tone::Failed);
    }
    #[test]
    fn ids_are_shown_short_and_rows_keep_the_full_id() {
        let full = "f".repeat(64);
        assert_eq!(short_id(format!("sha256:{full}")), "f".repeat(12));
        assert_eq!(short_id("abc".into()), "abc");
        let mut state = State::new(Default::default());
        state.native = Some(crate::native::parse("ps", &state).unwrap());
        state.data = serde_json::json!([{"ID": full, "Names": "web", "State": "running"}]);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 8)).unwrap();
        terminal
            .draw(|f| draw(f, f.area(), &state, Block::bordered()))
            .unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains(&"f".repeat(12)) && !text.contains(&"f".repeat(13)), "{text}");
        assert_eq!(state.selected().unwrap()["ID"], full);
    }
    #[test]
    fn resource_tables_display_cli_fields_and_keep_literal_untrusted_text() {
        let mut state = State::new(Default::default());
        state.native = Some(crate::native::parse("ps", &state).unwrap());
        state.data = serde_json::json!([{"ID":"abc", "Names":"web", "Image":"nginx", "Status":"Up", "Ports":"8080"}]);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 12)).unwrap();
        terminal
            .draw(|f| draw(f, f.area(), &state, Block::bordered().title("docker ps")))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        for value in [
            "ID", "NAMES", "IMAGE", "STATUS", "PORTS", "web", "nginx", "8080",
        ] {
            assert!(text.contains(value));
        }
        assert_eq!(super::text(&serde_json::json!("\u{1b}[2Jname")), "[2Jname");
    }
    #[test]
    fn container_size_column_honors_grouped_and_repeated_flags() {
        let mut state = State::new(Default::default());
        state.data = serde_json::json!([{"Names":"web", "Size":"42B"}]);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 12)).unwrap();
        for (flags, visible) in [
            ("", false),
            ("-as", true),
            ("-sa=false", true),
            ("-as=false", false),
            ("--size --size=false", false),
            ("--size=false -s", true),
            ("-sf label=a", true),
            ("--filter --size", false),
        ] {
            state.native = Some(crate::native::parse(&format!("ps {flags}"), &state).unwrap());
            terminal
                .draw(|f| draw(f, f.area(), &state, Block::bordered().title("containers")))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert_eq!(text.contains("SIZE"), visible, "{flags}");
            assert_eq!(text.contains("42B"), visible, "{flags}");
        }
    }
}
