use std::{
    collections::HashMap,
    env,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::Issue;

#[derive(Clone, Debug)]
pub struct AgentInfo {
    pub name: String,
    pub kind: String,
    pub status: String,
    // Live context size of the agent's session, when its harness exposes one
    // (filled in by claude::attach_context for Claude sessions).
    pub context_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Pi,
    Claude,
}

impl AgentKind {
    fn command_name(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::Claude => "claude",
        }
    }

    pub fn label(self, model: Option<&str>) -> String {
        match (self, model) {
            (Self::Claude, Some("claude-fable-5")) => "Claude · Fable".into(),
            (Self::Claude, Some("claude-opus-5")) => "Claude · Opus 5".into(),
            (Self::Claude, Some(model)) => format!("Claude · {model}"),
            (Self::Claude, None) => "Claude".into(),
            (Self::Pi, _) => "Pi".into(),
        }
    }
}

pub fn launch_agent(
    issue: &Issue,
    kind: AgentKind,
    model: Option<&str>,
    cwd: &Path,
) -> Result<String> {
    if env::var("HERDR_ENV").as_deref() != Ok("1") {
        bail!("Run beadsctrl inside a Herdr pane to launch agents");
    }

    let mut create = vec!["tab".into(), "create".into()];
    let workspace = workspace_for_project(cwd)
        .ok()
        .flatten()
        .or_else(|| env::var("HERDR_WORKSPACE_ID").ok());
    if let Some(workspace) = workspace {
        create.extend(["--workspace".into(), workspace]);
    }
    create.extend([
        "--cwd".into(),
        cwd.display().to_string(),
        "--label".into(),
        issue.id.clone(),
        "--no-focus".into(),
    ]);
    let pane = pane_id(&run_herdr(&create, cwd)?)?;
    let name = agent_name(&issue.id);

    let mut start = vec![
        "agent".into(),
        "start".into(),
        name.clone(),
        "--kind".into(),
        kind.command_name().into(),
        "--pane".into(),
        pane,
    ];
    if matches!(kind, AgentKind::Claude) {
        if let Some(model) = model {
            start.extend(["--".into(), "--model".into(), model.into()]);
        }
    }
    retry_start(&start, cwd)?;
    wait_for_interactive(&name, cwd);
    run_herdr(
        &[
            "agent".into(),
            "prompt".into(),
            name.clone(),
            task_prompt(issue),
        ],
        cwd,
    )?;
    run_herdr(&["agent".into(), "focus".into(), name.clone()], cwd)?;
    Ok(name)
}

pub fn discover_agents(issues: &[Issue], cwd: &Path) -> HashMap<String, AgentInfo> {
    if env::var("HERDR_ENV").as_deref() != Ok("1") {
        return HashMap::new();
    }
    let Ok(raw) = run_herdr(&["agent".into(), "list".into()], cwd) else {
        return HashMap::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return HashMap::new();
    };
    let Some(agents) = value["result"]["agents"].as_array() else {
        return HashMap::new();
    };
    let project = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut result = HashMap::new();
    for issue in issues {
        let prefix = format!(
            "{}-",
            safe_agent_base(&issue.id)
                .chars()
                .take(26)
                .collect::<String>()
        );
        let Some(agent) = agents.iter().find(|agent| {
            let same_project = agent["cwd"].as_str().is_some_and(|path| {
                Path::new(path)
                    .canonicalize()
                    .unwrap_or_else(|_| Path::new(path).to_path_buf())
                    == project
            });
            same_project
                && agent["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with(&prefix))
        }) else {
            continue;
        };
        let Some(name) = agent["name"].as_str() else {
            continue;
        };
        result.insert(
            issue.id.clone(),
            AgentInfo {
                name: name.into(),
                kind: agent["agent"].as_str().unwrap_or("agent").into(),
                status: agent["agent_status"].as_str().unwrap_or("unknown").into(),
                context_tokens: None,
            },
        );
    }
    result
}

pub fn read_agent_preview(name: &str, cwd: &Path) -> Result<Vec<String>> {
    let raw = run_herdr(
        &[
            "agent".into(),
            "read".into(),
            name.into(),
            "--source".into(),
            "recent-unwrapped".into(),
            "--lines".into(),
            "30".into(),
        ],
        cwd,
    )?;
    let mut lines = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim().trim_matches('│').trim();
        if trimmed.is_empty()
            || trimmed
                .chars()
                .all(|character| "─━═-╭╮╰╯│┌┐└┘⎿ ".contains(character))
            || trimmed.starts_with("~/")
            || trimmed.starts_with("/home/")
            || is_harness_chrome(trimmed)
        {
            continue;
        }
        if lines
            .last()
            .is_none_or(|previous: &String| previous != trimmed)
        {
            lines.push(trimmed.to_owned());
        }
    }
    if lines.len() > 6 {
        lines.drain(..lines.len() - 6);
    }
    Ok(lines)
}

// Terminal harnesses (Claude Code especially) fill the visible tail with
// status chrome — spinner lines, footer tips, the input box — which drowns
// out the transcript the preview is for.
fn is_harness_chrome(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.starts_with("tip:")
        || lower.starts_with("> ")
        || lower == ">"
        || lower.contains("to interrupt")
        || lower.contains("update available")
        || lower.contains("? for shortcuts")
        || lower.contains("shift+tab to cycle")
        || lower.contains("accept edits")
        || lower.contains("bypass permissions")
        || lower.contains("plan mode")
        || lower.contains("tokens remaining")
        || lower.contains("context left")
}

pub fn focus_agent(name: &str, cwd: &Path) -> Result<()> {
    run_herdr(&["agent".into(), "focus".into(), name.into()], cwd).map(drop)
}

fn workspace_for_project(cwd: &Path) -> Result<Option<String>> {
    let raw = run_herdr(&["workspace".into(), "list".into()], cwd)?;
    let value: Value =
        serde_json::from_str(&raw).context("herdr returned invalid workspace JSON")?;
    let workspaces = value["result"]["workspaces"]
        .as_array()
        .context("herdr workspace list had no workspaces")?;
    let project_name = cwd.file_name().and_then(|name| name.to_str()).unwrap_or("");

    let label_matches: Vec<_> = workspaces
        .iter()
        .filter(|workspace| {
            workspace["label"]
                .as_str()
                .is_some_and(|label| label.eq_ignore_ascii_case(project_name))
        })
        .filter_map(|workspace| workspace["workspace_id"].as_str().map(str::to_owned))
        .collect();
    if label_matches.len() == 1 {
        return Ok(label_matches.into_iter().next());
    }

    let project = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut scored = Vec::new();
    for workspace in workspaces {
        let Some(id) = workspace["workspace_id"].as_str() else {
            continue;
        };
        let Ok(raw) = run_herdr(
            &[
                "pane".into(),
                "list".into(),
                "--workspace".into(),
                id.into(),
            ],
            cwd,
        ) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let score = value["result"]["panes"]
            .as_array()
            .map(|panes| {
                panes
                    .iter()
                    .filter(|pane| {
                        [pane["cwd"].as_str(), pane["foreground_cwd"].as_str()]
                            .into_iter()
                            .flatten()
                            .any(|path| {
                                Path::new(path)
                                    .canonicalize()
                                    .unwrap_or_else(|_| Path::new(path).to_path_buf())
                                    == project
                            })
                    })
                    .count()
            })
            .unwrap_or(0);
        if score > 0 {
            scored.push((score, id.to_owned()));
        }
    }
    scored.sort_by(|left, right| right.0.cmp(&left.0));
    match scored.as_slice() {
        [(best_score, best), rest @ ..]
            if rest
                .first()
                .is_none_or(|(next_score, _)| next_score < best_score) =>
        {
            Ok(Some(best.clone()))
        }
        _ => Ok(None),
    }
}

fn task_prompt(issue: &Issue) -> String {
    format!(
        "Take on bead {}: {}. Start by running `bd show {}`, then atomically claim it with `bd update {} --claim`. Implement the bead in this repository and run the relevant checks. When the work is complete, review the diff and create a git commit containing only the changes for this bead with a clear commit message. Update/close the bead when appropriate.",
        issue.id, issue.title, issue.id, issue.id
    )
}

fn retry_start(args: &[String], cwd: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match run_herdr(args, cwd) {
            Ok(_) => return Ok(()),
            Err(error) => {
                let message = error.to_string();
                if !message.contains("agent_pane_busy") || Instant::now() >= deadline {
                    return Err(error);
                }
                thread::sleep(Duration::from_millis(300));
            }
        }
    }
}

fn wait_for_interactive(name: &str, cwd: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok(raw) = run_herdr(&["agent".into(), "list".into()], cwd)
            && let Ok(value) = serde_json::from_str::<Value>(&raw)
            && value["result"]["agents"].as_array().is_some_and(|agents| {
                agents.iter().any(|agent| {
                    agent["name"].as_str() == Some(name)
                        && agent["interactive_ready"].as_bool() == Some(true)
                })
            })
        {
            return;
        }
        thread::sleep(Duration::from_millis(300));
    }
}

fn run_herdr(args: &[String], cwd: &Path) -> Result<String> {
    let output = Command::new("herdr")
        .args(args)
        .current_dir(cwd)
        .output()
        .context("could not run herdr")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let raw = if stderr.is_empty() { stdout } else { stderr };
        // herdr reports failures as {"error":{"code","message"}} JSON; keep the
        // code in front so retry_start can still match on it.
        let message = serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|value| {
                let code = value["error"]["code"].as_str()?.to_owned();
                match value["error"]["message"].as_str() {
                    Some(message) => Some(format!("{code}: {message}")),
                    None => Some(code),
                }
            })
            .unwrap_or(raw);
        bail!("{message}");
    }
    String::from_utf8(output.stdout).context("herdr returned non-UTF-8 output")
}

fn pane_id(raw: &str) -> Result<String> {
    let value: Value = serde_json::from_str(raw).context("herdr returned invalid JSON")?;
    value["result"]["pane"]["pane_id"]
        .as_str()
        .or_else(|| value["result"]["root_pane"]["pane_id"].as_str())
        .map(str::to_owned)
        .context("herdr did not return a pane ID")
}

fn safe_agent_base(issue_id: &str) -> String {
    let mut base: String = issue_id
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || "_-".contains(character) {
                character
            } else {
                '-'
            }
        })
        .collect();
    if !base.starts_with(|character: char| character.is_ascii_alphabetic()) {
        base = format!("bead-{base}");
    }
    base
}

fn agent_name(issue_id: &str) -> String {
    let mut base = safe_agent_base(issue_id);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let stamp = base36(millis);
    let suffix = format!("-{}", &stamp[stamp.len().saturating_sub(5)..]);
    base.truncate(32usize.saturating_sub(suffix.len()));
    format!("{base}{suffix}")
}

fn base36(mut value: u64) -> String {
    let mut output = Vec::new();
    loop {
        let digit = (value % 36) as u8;
        output.push(if digit < 10 {
            b'0' + digit
        } else {
            b'a' + digit - 10
        });
        value /= 36;
        if value == 0 {
            break;
        }
    }
    output.reverse();
    String::from_utf8(output).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_names_are_safe_and_bounded() {
        let name = agent_name("12/A Very Long Bead Identifier With Spaces");
        assert!(name.starts_with("bead-12-a-very"));
        assert!(name.len() <= 32);
    }
}
