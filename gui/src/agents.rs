use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::{
    claude::{self, SessionIndex},
    herdr::{self, AgentInfo, Discovery, PaneAgent},
    model::DashboardData,
};

// How far a transcript's creation may sit from its process's start before we
// stop believing they belong together.
const START_SKEW: u64 = 120;
// Two transcripts born this close to a process start are indistinguishable.
const AMBIGUOUS: u64 = 10;

// Which agent is on which bead. Launched agents are known by the name we gave
// them; tabs somebody opened by hand are attributed from the claim in their
// transcript. Reading a pane's screen would work too, but `herdr agent read`
// disturbs the pane's viewport, so we never scrape as a fallback.
#[derive(Default)]
pub struct AgentScan {
    sessions: SessionIndex,
}

impl AgentScan {
    pub fn run(&mut self, data: &DashboardData, project: &Path) -> HashMap<String, AgentInfo> {
        let mut discovery = herdr::discover_agents(&data.issues, project);
        self.attribute(&mut discovery, data);
        claude::attach_context(&mut discovery.by_bead, project, &mut self.sessions);
        discovery.by_bead
    }

    fn attribute(&mut self, discovery: &mut Discovery, data: &DashboardData) {
        let known: HashSet<&str> = data.issues.iter().map(|issue| issue.id.as_str()).collect();
        // Only claimed beads that no launched agent accounts for are up for
        // attribution — an in-progress bead with nobody on it is exactly what
        // an untracked tab looks like from here.
        let mut wanted: HashSet<&str> = data
            .issues
            .iter()
            .filter(|issue| {
                issue.status == "in_progress" && !discovery.by_bead.contains_key(&issue.id)
            })
            .map(|issue| issue.id.as_str())
            .collect();
        if wanted.is_empty() || discovery.unclaimed.is_empty() {
            return;
        }

        let transcripts = transcripts_for(&discovery.unclaimed);
        for pane in &discovery.unclaimed {
            let Some(path) = transcripts.get(&pane.target) else {
                continue;
            };
            let Some(bead) = self.sessions.claimed_bead(path, &known) else {
                continue;
            };
            if !wanted.remove(bead.as_str()) {
                continue;
            }
            let mut info = pane.info(true);
            info.context_tokens = claude::context_tokens(path);
            discovery.by_bead.insert(bead, info);
        }
    }
}

// Transcript file for each Claude pane that has one, keyed by herdr target.
fn transcripts_for(panes: &[PaneAgent]) -> HashMap<String, PathBuf> {
    let mut paired = HashMap::new();
    let mut unpaired = Vec::new();
    for pane in panes.iter().filter(|pane| pane.kind.contains("claude")) {
        // Reported by the harness itself when `herdr integration install
        // claude` is in place: exact, and nothing else to work out.
        match &pane.session_path {
            Some(path) => {
                paired.insert(pane.target.clone(), path.clone());
            }
            None => unpaired.push(pane),
        }
    }
    if unpaired.is_empty() {
        return paired;
    }

    let processes = pane_processes();
    let mut proposals = Vec::new();
    for pane in unpaired {
        let Some(process) = processes.iter().find(|process| process.pane == pane.pane) else {
            continue;
        };
        proposals.extend(
            candidate_transcripts(process)
                .into_iter()
                .map(|(score, path)| (score, pane.target.clone(), path)),
        );
    }
    proposals.sort_by_key(|(score, _, _)| *score);

    let mut used: HashSet<PathBuf> = paired.values().cloned().collect();
    for (_, target, path) in proposals {
        if paired.contains_key(&target) || !used.insert(path.clone()) {
            continue;
        }
        paired.insert(target, path);
    }
    paired
}

// Transcripts in the pane's project directory that this process could have
// written, scored by how close their creation sits to the process start.
fn candidate_transcripts(process: &PaneProcess) -> Vec<(u64, PathBuf)> {
    let Some(dir) = claude::transcript_dir(&process.cwd) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut candidates: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "jsonl") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            // Only a file still being written while this process has been
            // running can belong to it.
            if metadata.modified().ok()? < process.started {
                return None;
            }
            let score = metadata
                .created()
                .ok()
                .map_or(u64::MAX, |created| gap(created, process.started));
            Some((score, path))
        })
        .collect();
    candidates.sort();
    match candidates.as_slice() {
        // A resumed session keeps its original creation time, so the only
        // live transcript in the directory is still the answer.
        [only] => vec![only.clone()],
        [best, next, ..] if best.0 <= START_SKEW && next.0 > best.0 + AMBIGUOUS => vec![best.clone()],
        // Two sessions opened at once in one directory: refuse to guess.
        _ => Vec::new(),
    }
}

fn gap(left: SystemTime, right: SystemTime) -> u64 {
    left.duration_since(right)
        .or_else(|_| right.duration_since(left))
        .map_or(u64::MAX, |delta| delta.as_secs())
}

struct PaneProcess {
    pane: String,
    cwd: PathBuf,
    started: SystemTime,
}

// Herdr exports HERDR_PANE_ID into every pane, so an agent process says which
// pane it lives in; /proc/<pid> is stamped with when the process started,
// which is what pairs it with the transcript it opened.
fn pane_processes() -> Vec<PaneProcess> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            if !entry.file_name().to_string_lossy().starts_with(|c: char| c.is_ascii_digit()) {
                return None;
            }
            let dir = entry.path();
            // Everything else in the pane — the shell, our own bd calls —
            // has no transcript to pair.
            if fs::read_to_string(dir.join("comm")).ok()?.trim() != "claude" {
                return None;
            }
            let environ = fs::read(dir.join("environ")).ok()?;
            let pane = environ.split(|byte| *byte == 0).find_map(|variable| {
                std::str::from_utf8(variable)
                    .ok()?
                    .strip_prefix("HERDR_PANE_ID=")
                    .map(str::to_owned)
            })?;
            Some(PaneProcess {
                pane,
                cwd: fs::read_link(dir.join("cwd")).ok()?,
                started: fs::metadata(&dir).ok()?.modified().ok()?,
            })
        })
        .collect()
}

