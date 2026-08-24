use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Dependency {
    pub issue_id: String,
    pub depends_on_id: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Issue {
    pub id: String,
    #[serde(default = "untitled")]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "open")]
    pub status: String,
    #[serde(default = "default_priority")]
    pub priority: u8,
    #[serde(default = "task")]
    pub issue_type: String,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
}

fn untitled() -> String {
    "Untitled".into()
}
fn open() -> String {
    "open".into()
}
fn task() -> String {
    "task".into()
}
fn default_priority() -> u8 {
    2
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkState {
    Ready,
    Blocked,
    InProgress,
    Closed,
    Other,
}

#[derive(Clone, Debug)]
pub struct EpicSummary {
    pub epic: Issue,
    pub children: Vec<Issue>,
    pub closed: usize,
}

impl EpicSummary {
    pub fn progress(&self) -> f32 {
        if self.children.is_empty() {
            0.0
        } else {
            self.closed as f32 / self.children.len() as f32
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DashboardData {
    pub issues: Vec<Issue>,
    pub epics: Vec<EpicSummary>,
    pub ungrouped: Vec<Issue>,
    pub states: HashMap<String, WorkState>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub open: usize,
    pub ready: usize,
    pub in_progress: usize,
    pub closed: usize,
    pub total: usize,
}

impl DashboardData {
    pub fn from_export(raw: &str) -> Result<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(Self::default());
        }

        let issues: Vec<Issue> = if trimmed.starts_with('[') {
            serde_json::from_str(trimmed).context("invalid bd export JSON")?
        } else {
            trimmed
                .lines()
                .enumerate()
                .map(|(index, line)| {
                    serde_json::from_str(line)
                        .with_context(|| format!("invalid issue on export line {}", index + 1))
                })
                .collect::<Result<_>>()?
        };
        Ok(Self::new(issues))
    }

    pub fn new(issues: Vec<Issue>) -> Self {
        let by_id: HashMap<&str, &Issue> = issues
            .iter()
            .map(|issue| (issue.id.as_str(), issue))
            .collect();
        let mut parent_by_child = HashMap::<&str, &str>::new();
        let mut blockers = HashMap::<&str, Vec<&str>>::new();
        for issue in &issues {
            for dependency in &issue.dependencies {
                if dependency.kind == "parent-child" {
                    parent_by_child.insert(
                        dependency.issue_id.as_str(),
                        dependency.depends_on_id.as_str(),
                    );
                } else if dependency.kind == "blocks" {
                    blockers
                        .entry(dependency.issue_id.as_str())
                        .or_default()
                        .push(dependency.depends_on_id.as_str());
                }
            }
        }

        let states = issues
            .iter()
            .map(|issue| {
                let state = match issue.status.as_str() {
                    "closed" => WorkState::Closed,
                    "in_progress" => WorkState::InProgress,
                    "open" => {
                        let blocked = blockers.get(issue.id.as_str()).is_some_and(|ids| {
                            ids.iter().any(|id| {
                                by_id
                                    .get(id)
                                    .is_none_or(|blocker| blocker.status != "closed")
                            })
                        });
                        if blocked {
                            WorkState::Blocked
                        } else {
                            WorkState::Ready
                        }
                    }
                    _ => WorkState::Other,
                };
                (issue.id.clone(), state)
            })
            .collect();

        let epic_ids: HashSet<&str> = issues
            .iter()
            .filter(|issue| issue.issue_type == "epic")
            .map(|issue| issue.id.as_str())
            .collect();
        let mut children_by_epic = HashMap::<&str, Vec<Issue>>::new();
        let mut grouped = HashSet::<&str>::new();

        for issue in issues.iter().filter(|issue| issue.issue_type != "epic") {
            let mut parent = parent_by_child.get(issue.id.as_str()).copied();
            let mut seen = HashSet::from([issue.id.as_str()]);
            while let Some(parent_id) = parent {
                if !seen.insert(parent_id) {
                    break;
                }
                if epic_ids.contains(parent_id) {
                    children_by_epic
                        .entry(parent_id)
                        .or_default()
                        .push(issue.clone());
                    grouped.insert(issue.id.as_str());
                    break;
                }
                parent = parent_by_child.get(parent_id).copied();
            }
        }

        let mut epics: Vec<_> = issues
            .iter()
            .filter(|issue| issue.issue_type == "epic")
            .map(|epic| {
                let mut children = children_by_epic
                    .remove(epic.id.as_str())
                    .unwrap_or_default();
                sort_issues(&mut children);
                let closed = children
                    .iter()
                    .filter(|child| child.status == "closed")
                    .count();
                EpicSummary {
                    epic: epic.clone(),
                    children,
                    closed,
                }
            })
            .collect();
        epics.sort_by_key(|summary| {
            (
                summary.epic.status == "closed",
                summary.epic.priority,
                summary.epic.title.to_lowercase(),
            )
        });

        let mut ungrouped: Vec<_> = issues
            .iter()
            .filter(|issue| issue.issue_type != "epic" && !grouped.contains(issue.id.as_str()))
            .cloned()
            .collect();
        sort_issues(&mut ungrouped);

        Self {
            issues,
            epics,
            ungrouped,
            states,
        }
    }

    pub fn state(&self, id: &str) -> WorkState {
        self.states.get(id).copied().unwrap_or(WorkState::Other)
    }

    pub fn issue(&self, id: &str) -> Option<&Issue> {
        self.issues.iter().find(|issue| issue.id == id)
    }

    pub fn stats(&self) -> Stats {
        let mut stats = Stats {
            total: self.issues.len(),
            ..Stats::default()
        };
        for issue in &self.issues {
            match self.state(&issue.id) {
                WorkState::Ready => {
                    stats.ready += 1;
                    stats.open += 1;
                }
                WorkState::Blocked => stats.open += 1,
                WorkState::InProgress => stats.in_progress += 1,
                WorkState::Closed => stats.closed += 1,
                WorkState::Other => {}
            }
        }
        stats
    }
}

fn sort_issues(issues: &mut [Issue]) {
    issues.sort_by_key(|issue| {
        (
            issue.status == "closed",
            issue.priority,
            issue.title.to_lowercase(),
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_nested_epics_progress_and_readiness() {
        let raw = r#"
{"id":"e","title":"Epic","status":"open","priority":1,"issue_type":"epic"}
{"id":"a","title":"Parent","status":"closed","priority":2,"issue_type":"task","dependencies":[{"issue_id":"a","depends_on_id":"e","type":"parent-child"}]}
{"id":"b","title":"Nested","status":"open","priority":0,"issue_type":"task","dependencies":[{"issue_id":"b","depends_on_id":"a","type":"parent-child"},{"issue_id":"b","depends_on_id":"a","type":"blocks"}]}
{"id":"c","title":"Loose","status":"open","priority":2,"issue_type":"task"}
"#;
        let data = DashboardData::from_export(raw).unwrap();
        assert_eq!(data.epics[0].children.len(), 2);
        assert_eq!(data.epics[0].closed, 1);
        assert_eq!(data.state("b"), WorkState::Ready);
        assert_eq!(data.ungrouped[0].id, "c");
    }
}
