use crate::{preferences::Workspace, tui_state::State};
use ratatui::{Frame, layout::{Constraint, Rect}, style::{Color, Style}, widgets::{Block, Cell, Row, Table, TableState}};
use serde_json::Value;
fn text(value: &Value) -> String { crate::tui_state::clean(value.as_str().map(String::from).unwrap_or_else(|| if value.is_null() { String::new() } else { value.to_string() }).as_str()) }
pub fn draw(frame: &mut Frame, area: Rect, state: &State, title: String) {
    let invocation = state.native.as_ref().unwrap();
    let columns: &[&str] = if invocation.workspace == Workspace::Kubernetes { &["Name", "Namespace", "Status"] }
        else { match invocation.resource.as_deref() {
            Some("images") => &["Repository", "Tag", "ID", "Size"],
            Some("volumes") => &["Name", "Driver", "Scope"],
            Some("networks") => &["ID", "Name", "Driver", "Scope"],
            _ => &["ID", "Names", "Image", "Status", "Ports"],
        }};
    let rows: Vec<Row> = state.rows().into_iter().map(|row| {
        let fields: Vec<Cell> = if invocation.workspace == Workspace::Kubernetes {
            let status = if !row["status"]["phase"].is_null() { text(&row["status"]["phase"]) }
                else if !row["spec"]["clusterIP"].is_null() { format!("{} {}", text(&row["spec"]["type"]), text(&row["spec"]["clusterIP"])) }
                else if !row["spec"]["replicas"].is_null() { format!("{}/{} ready", row["status"]["readyReplicas"].as_u64().unwrap_or(0), row["spec"]["replicas"]) }
                else { String::new() };
            vec![text(&row["metadata"]["name"]).into(), text(&row["metadata"]["namespace"]).into(), status.into()]
        } else {
            columns.iter().map(|name| Cell::from(text(if *name == "Status" && row[name].is_null() { &row["State"] } else { &row[name] }))).collect()
        };
        Row::new(fields)
    }).collect();
    let widths: Vec<_> = columns.iter().map(|_| Constraint::Fill(1)).collect();
    let table = Table::new(rows, widths).header(Row::new(columns.iter().copied()).style(Style::default().fg(Color::DarkGray)))
        .block(Block::bordered().title(title)).row_highlight_style(Style::default().fg(Color::Cyan)).highlight_symbol("> ");
    frame.render_stateful_widget(table, area, &mut TableState::default().with_selected(Some(state.selected)));
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_tables_display_cli_fields_and_keep_literal_untrusted_text() {
        let mut state = State::new(Default::default());
        state.native = Some(crate::native::parse("ps", &state).unwrap());
        state.data = serde_json::json!([{"ID":"abc", "Names":"web", "Image":"nginx", "Status":"Up", "Ports":"8080"}]);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 12)).unwrap();
        terminal.draw(|f| draw(f, f.area(), &state, "docker ps".into())).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        for value in ["ID", "Names", "Image", "Status", "Ports", "web", "nginx", "8080"] { assert!(text.contains(value)); }
        assert_eq!(super::text(&serde_json::json!("\u{1b}[2Jname")), "[2Jname");
    }
}
