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
    #[serde(default)]
    pub closed_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
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
    // blocker id -> ids of the beads it blocks
    pub dependents: HashMap<String, Vec<String>>,
    // blocked id -> ids of the beads that block it
    pub blocked_by: HashMap<String, Vec<String>>,
}

// One row of the flattened blocks tree shown in the inspector: the prefix
// carries the box-drawing glyphs so rendering is a plain list of lines.
#[derive(Clone, Debug, PartialEq)]
pub struct BlocksNode {
    pub id: String,
    pub prefix: String,
    pub cycle: bool,
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
        let mut dependents = HashMap::<String, Vec<String>>::new();
        let mut blocked_by = HashMap::<String, Vec<String>>::new();
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
                    let blocked = dependents
                        .entry(dependency.depends_on_id.clone())
                        .or_default();
                    if !blocked.contains(&dependency.issue_id) {
                        blocked.push(dependency.issue_id.clone());
                    }
                    let blocking = blocked_by.entry(dependency.issue_id.clone()).or_default();
                    if !blocking.contains(&dependency.depends_on_id) {
                        blocking.push(dependency.depends_on_id.clone());
                    }
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
            dependents,
            blocked_by,
        }
    }

    // What this bead is holding up: dependents of `root`, nested recursively.
    pub fn blocks_tree(&self, root: &str) -> Vec<BlocksNode> {
        self.dependency_tree(root, &self.dependents)
    }

    // What is holding this bead up: its blockers, and their blockers, nested
    // recursively.
    pub fn blocked_by_tree(&self, root: &str) -> Vec<BlocksNode> {
        self.dependency_tree(root, &self.blocked_by)
    }

    fn dependency_tree(&self, root: &str, edges: &HashMap<String, Vec<String>>) -> Vec<BlocksNode> {
        let mut nodes = Vec::new();
        let children = self.sorted_edges(edges, root);
        let ancestors = HashSet::from([root.to_owned()]);
        for (index, child) in children.iter().enumerate() {
            self.push_tree_node(
                edges,
                child,
                "",
                index == children.len() - 1,
                &ancestors,
                &mut nodes,
            );
        }
        nodes
    }

    // Open work first, then priority, then id — closed beads sink so the tree
    // leads with what is still waiting.
    fn sorted_edges(&self, edges: &HashMap<String, Vec<String>>, id: &str) -> Vec<String> {
        let mut children = edges.get(id).cloned().unwrap_or_default();
        children.sort_by(|a, b| {
            let left = self.issue(a);
            let right = self.issue(b);
            let rank = |issue: Option<&Issue>| {
                (
                    issue.is_some_and(|issue| issue.status == "closed"),
                    issue.map_or(99, |issue| issue.priority),
                )
            };
            rank(left).cmp(&rank(right)).then_with(|| a.cmp(b))
        });
        children
    }

    fn push_tree_node(
        &self,
        edges: &HashMap<String, Vec<String>>,
        id: &str,
        prefix: &str,
        last: bool,
        ancestors: &HashSet<String>,
        nodes: &mut Vec<BlocksNode>,
    ) {
        let cycle = ancestors.contains(id);
        nodes.push(BlocksNode {
            id: id.to_owned(),
            prefix: format!("{prefix}{} ", if last { "└─" } else { "├─" }),
            cycle,
        });
        if cycle {
            return;
        }
        let children = self.sorted_edges(edges, id);
        if children.is_empty() {
            return;
        }
        let mut next_ancestors = ancestors.clone();
        next_ancestors.insert(id.to_owned());
        let next_prefix = format!("{prefix}{}", if last { "   " } else { "│  " });
        for (index, child) in children.iter().enumerate() {
            self.push_tree_node(
                edges,
                child,
                &next_prefix,
                index == children.len() - 1,
                &next_ancestors,
                nodes,
            );
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

    #[test]
    fn blocks_tree_nests_sorts_and_flags_cycles() {
        let raw = r#"
{"id":"root","title":"Root","status":"open","priority":1,"issue_type":"task"}
{"id":"closed","title":"Done","status":"closed","priority":0,"issue_type":"task","dependencies":[{"issue_id":"closed","depends_on_id":"root","type":"blocks"}]}
{"id":"hot","title":"Hot","status":"open","priority":0,"issue_type":"task","dependencies":[{"issue_id":"hot","depends_on_id":"root","type":"blocks"}]}
{"id":"leaf","title":"Leaf","status":"open","priority":2,"issue_type":"task","dependencies":[{"issue_id":"leaf","depends_on_id":"hot","type":"blocks"},{"issue_id":"leaf","depends_on_id":"hot","type":"blocks"}]}
"#;
        let data = DashboardData::from_export(raw).unwrap();
        let nodes = data.blocks_tree("root");
        let rows: Vec<_> = nodes
            .iter()
            .map(|node| format!("{}{}", node.prefix, node.id))
            .collect();
        // Open beads first, closed last; leaf nests under hot exactly once.
        assert_eq!(rows, vec!["├─ hot", "│  └─ leaf", "└─ closed"]);

        let cyclic = DashboardData::from_export(
            r#"
{"id":"a","title":"A","status":"open","priority":1,"issue_type":"task","dependencies":[{"issue_id":"a","depends_on_id":"b","type":"blocks"}]}
{"id":"b","title":"B","status":"open","priority":1,"issue_type":"task","dependencies":[{"issue_id":"b","depends_on_id":"a","type":"blocks"}]}
"#,
        )
        .unwrap();
        let nodes = cyclic.blocks_tree("a");
        assert_eq!(nodes.len(), 2);
        assert!(nodes[1].cycle, "revisiting the root must stop the walk");
    }

    #[test]
    fn blocked_by_tree_walks_the_reverse_edge() {
        let raw = r#"
{"id":"root","title":"Root","status":"open","priority":1,"issue_type":"task"}
{"id":"hot","title":"Hot","status":"open","priority":0,"issue_type":"task","dependencies":[{"issue_id":"hot","depends_on_id":"root","type":"blocks"}]}
{"id":"leaf","title":"Leaf","status":"open","priority":2,"issue_type":"task","dependencies":[{"issue_id":"leaf","depends_on_id":"hot","type":"blocks"}]}
"#;
        let data = DashboardData::from_export(raw).unwrap();
        let nodes = data.blocked_by_tree("leaf");
        let rows: Vec<_> = nodes
            .iter()
            .map(|node| format!("{}{}", node.prefix, node.id))
            .collect();
        assert_eq!(rows, vec!["└─ hot", "   └─ root"]);
        assert!(data.blocked_by_tree("root").is_empty());
    }
}
