use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    herdr::AgentKind,
    model::{DashboardData, WorkState},
};

// Shared with the TUI: same JSON shape, same file, so a bead queued in either
// frontend shows up in both. `model: None` is omitted on write, matching
// JSON.stringify's treatment of undefined.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub cwd: String,
    pub id: String,
    pub kind: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub at: u64,
}

pub fn canonical_project(project: &Path) -> PathBuf {
    project.canonicalize().unwrap_or_else(|_| project.to_path_buf())
}

pub fn load_queue(project: &Path) -> Vec<QueueEntry> {
    let cwd = canonical_project(project);
    let Ok(raw) = fs::read_to_string(state_file_path(project, "queue.json")) else {
        return Vec::new();
    };
    let Ok(entries) = serde_json::from_str::<Vec<QueueEntry>>(&raw) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .filter(|entry| Path::new(&entry.cwd) == cwd)
        .collect()
}

pub fn save_queue(project: &Path, queue: &[QueueEntry]) -> Result<()> {
    let path = state_file_path(project, "queue.json");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("could not create state directory")?;
    }
    let json = serde_json::to_string_pretty(queue).context("could not encode queue")?;
    fs::write(&path, json + "\n").context("could not write queue file")
}

// A queued bead that somebody claimed or closed in the meantime is no longer
// ours to launch. Unknown IDs are kept: the issue graph may not be loaded yet.
pub fn prune(queue: &mut Vec<QueueEntry>, data: &DashboardData) -> bool {
    let before = queue.len();
    queue.retain(|entry| {
        data.issue(&entry.id)
            .is_none_or(|bead| bead.status != "closed" && bead.status != "in_progress")
    });
    queue.len() != before
}

// The whole point of the queue: nothing starts while an agent is still
// working, so queued beads never share the working tree with another agent.
pub fn next_ready<'a>(queue: &'a [QueueEntry], data: &DashboardData) -> Option<&'a QueueEntry> {
    queue
        .iter()
        .find(|entry| data.state(&entry.id) == WorkState::Ready)
}

pub fn position(queue: &[QueueEntry], bead_id: &str) -> Option<usize> {
    queue.iter().position(|entry| entry.id == bead_id)
}

// Scans an agent may be missing from before its claim is written off. An
// agent that vanishes without claiming — tab closed, harness died, permission
// prompt declined — would otherwise hold the deck for the rest of the
// session. Several ticks, so a scan that started before the tab existed, or a
// herdr call that failed once, never releases the tree out from under a live
// agent.
pub const CLAIM_MISSES: u8 = 3;

// A bead we launched an agent for that has not yet run `bd update --claim`.
// It counts as working so the queue never double-launches into the gap
// between agent start and the claim landing in bd.
#[derive(Debug)]
pub struct Claim {
    // The queue entry this launch came from, so work is put back where it was
    // if the launch never lands. Hand-started launches have nothing to
    // restore.
    pub entry: Option<QueueEntry>,
    // False until the launch returns: an in-flight launch has no tab to look
    // for yet.
    pub launched: bool,
    // Consecutive scans since the launch returned that did not see the agent.
    pub misses: u8,
}

impl Claim {
    pub fn new(entry: Option<QueueEntry>) -> Self {
        Self { entry, launched: false, misses: 0 }
    }
}

// Drops claims bd has confirmed (or that closed), and expires the ones whose
// agent has been gone for CLAIM_MISSES scans running. Beads bd has never
// heard of are kept: the issue graph may not be loaded yet. Returns the
// expired claims — the caller decides what to do with the work they were
// holding.
pub fn expire_claims(
    claims: &mut HashMap<String, Claim>,
    data: &DashboardData,
    present: impl Fn(&str) -> bool,
) -> Vec<(String, Claim)> {
    let mut confirmed = Vec::new();
    let mut expired = Vec::new();
    for (id, claim) in claims.iter_mut() {
        if data.issue(id).is_some_and(|bead| bead.status != "open") {
            confirmed.push(id.clone());
        } else if !claim.launched || present(id) {
            claim.misses = 0;
        } else {
            claim.misses += 1;
            if claim.misses >= CLAIM_MISSES {
                expired.push(id.clone());
            }
        }
    }
    for id in confirmed {
        claims.remove(&id);
    }
    expired
        .into_iter()
        .filter_map(|id| claims.remove(&id).map(|claim| (id, claim)))
        .collect()
}

// GUI-only preferences (the auto/manual toggle) live next to the shared queue
// file but never touch it, so the TUI stays oblivious.
#[derive(Serialize, Deserialize)]
struct GuiPrefs {
    auto: bool,
}

pub fn load_auto(project: &Path) -> bool {
    fs::read_to_string(state_file_path(project, "gui.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<GuiPrefs>(&raw).ok())
        .map(|prefs| prefs.auto)
        .unwrap_or(false)
}

pub fn save_auto(project: &Path, auto: bool) -> Result<()> {
    let path = state_file_path(project, "gui.json");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("could not create state directory")?;
    }
    let json = serde_json::to_string_pretty(&GuiPrefs { auto }).context("could not encode prefs")?;
    fs::write(&path, json + "\n").context("could not write prefs file")
}

// Mirrors the TUI's stateFilePath (tui/src/herdr.ts): one file per project,
// named by Bun.hash of the canonical path in base36.
pub fn state_file_path(project: &Path, name: &str) -> PathBuf {
    let root = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            home.join(".local").join("state")
        })
        .join("beadsviewer");
    let cwd = canonical_project(project);
    let hash = wyhash(cwd.to_string_lossy().as_bytes(), 0);
    root.join(format!("{}-{name}", base36(hash)))
}

fn base36(mut value: u64) -> String {
    if value == 0 {
        return "0".into();
    }
    let mut output = Vec::new();
    while value > 0 {
        let digit = (value % 36) as u8;
        output.push(if digit < 10 { b'0' + digit } else { b'a' + digit - 10 });
        value /= 36;
    }
    output.reverse();
    String::from_utf8(output).unwrap_or_default()
}

// Wyhash (final 4), the algorithm behind Zig's std.hash.Wyhash and therefore
// Bun.hash — verified against Bun.hash output in the tests below.
const SECRET: [u64; 4] = [
    0xa0761d6478bd642f,
    0xe7037ed1a0b428db,
    0x8ebc6af09c88c6e3,
    0x589965cc75374cc3,
];

fn wymix(a: u64, b: u64) -> u64 {
    let product = u128::from(a) * u128::from(b);
    (product as u64) ^ ((product >> 64) as u64)
}

fn read8(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

fn read4(data: &[u8], offset: usize) -> u64 {
    u64::from(u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()))
}

fn wyhash(data: &[u8], seed: u64) -> u64 {
    let len = data.len();
    let mut state = seed ^ wymix(seed ^ SECRET[0], SECRET[1]);
    let (mut a, mut b);
    if len <= 16 {
        if len >= 4 {
            let end = len - 4;
            let quarter = (len >> 3) << 2;
            a = (read4(data, 0) << 32) | read4(data, quarter);
            b = (read4(data, end) << 32) | read4(data, end - quarter);
        } else if len > 0 {
            a = (u64::from(data[0]) << 16)
                | (u64::from(data[len >> 1]) << 8)
                | u64::from(data[len - 1]);
            b = 0;
        } else {
            a = 0;
            b = 0;
        }
    } else {
        let mut offset = 0;
        if len > 48 {
            let mut lanes = [state; 3];
            while offset + 48 < len {
                for (lane, chunk) in lanes.iter_mut().zip(0..3) {
                    *lane = wymix(
                        read8(data, offset + 16 * chunk) ^ SECRET[chunk + 1],
                        read8(data, offset + 16 * chunk + 8) ^ *lane,
                    );
                }
                offset += 48;
            }
            state = lanes[0] ^ lanes[1] ^ lanes[2];
        }
        let mut i = 0;
        while offset + i + 16 < len {
            state = wymix(
                read8(data, offset + i) ^ SECRET[1],
                read8(data, offset + i + 8) ^ state,
            );
            i += 16;
        }
        a = read8(data, len - 16);
        b = read8(data, len - 8);
    }
    a ^= SECRET[1];
    b ^= state;
    let product = u128::from(a) * u128::from(b);
    a = product as u64;
    b = (product >> 64) as u64;
    wymix(a ^ SECRET[0] ^ len as u64, b ^ SECRET[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Issue;

    // Ground truth generated on this machine with
    // `bun -e 'console.log(Bun.hash(s).toString(36))'`.
    #[test]
    fn wyhash_matches_bun_hash() {
        let vectors = [
            ("", "27k1wwwhf13t"),
            ("a", "mcie3cqm6cz5"),
            ("abc", "1g45uqqks6lu"),
            ("/home/alec/code/bdctrl", "xqd87ekdgpa2"),
            (
                "hello world this is a longer string to exercise the loop path over 48 bytes total!",
                "2pdpzu5sx1a4i",
            ),
            ("/tmp/x", "iefvindl102c"),
        ];
        for (input, expected) in vectors {
            assert_eq!(base36(wyhash(input.as_bytes(), 0)), expected, "input: {input:?}");
        }
        let boundary_lengths = [
            (16, "6afk8manhkh9"),
            (17, "1vophv09lvqpv"),
            (48, "2aqe7o80phxou"),
            (49, "3pc7qi655mct1"),
            (52, "1pwsw55nw6xdc"),
            (96, "33oo1bxurxubm"),
        ];
        for (length, expected) in boundary_lengths {
            let input = "x".repeat(length);
            assert_eq!(base36(wyhash(input.as_bytes(), 0)), expected, "length: {length}");
        }
    }

    #[test]
    fn queue_json_round_trips_tui_format() {
        let raw = r#"[
  {
    "cwd": "/home/alec/code/bdctrl",
    "id": "bd-9",
    "kind": "claude",
    "model": "claude-fable-5",
    "at": 1755900000000
  },
  {
    "cwd": "/home/alec/code/bdctrl",
    "id": "bd-31",
    "kind": "pi",
    "at": 1755900000001
  }
]"#;
        let entries: Vec<QueueEntry> = serde_json::from_str(raw).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(matches!(entries[0].kind, AgentKind::Claude));
        assert!(matches!(entries[1].kind, AgentKind::Pi));
        assert_eq!(entries[1].model, None);
        assert_eq!(serde_json::to_string_pretty(&entries).unwrap(), raw);
    }

    fn issue(id: &str, status: &str, blocked_on: Option<&str>) -> Issue {
        let dependencies = blocked_on
            .map(|blocker| {
                serde_json::from_str(&format!(
                    r#"[{{"issue_id":"{id}","depends_on_id":"{blocker}","type":"blocks"}}]"#
                ))
                .unwrap()
            })
            .unwrap_or_default();
        serde_json::from_str::<Issue>(&format!(
            r#"{{"id":"{id}","title":"{id}","status":"{status}"}}"#
        ))
        .map(|mut issue| {
            issue.dependencies = dependencies;
            issue
        })
        .unwrap()
    }

    fn entry(id: &str) -> QueueEntry {
        QueueEntry {
            cwd: "/p".into(),
            id: id.into(),
            kind: AgentKind::Claude,
            model: None,
            at: 0,
        }
    }

    #[test]
    fn prunes_claimed_and_closed_keeps_unknown() {
        let data = DashboardData::new(vec![
            issue("open", "open", None),
            issue("done", "closed", None),
            issue("busy", "in_progress", None),
        ]);
        let mut queue = vec![entry("open"), entry("done"), entry("busy"), entry("mystery")];
        assert!(prune(&mut queue, &data));
        let ids: Vec<_> = queue.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, ["open", "mystery"]);
    }

    fn claims(ids: [(&str, bool); 1]) -> HashMap<String, Claim> {
        ids.into_iter()
            .map(|(id, launched)| {
                let mut claim = Claim::new(Some(entry(id)));
                claim.launched = launched;
                (id.to_string(), claim)
            })
            .collect()
    }

    #[test]
    fn claim_is_held_while_the_launch_is_still_in_flight() {
        let data = DashboardData::new(vec![issue("bd-1", "open", None)]);
        let mut held = claims([("bd-1", false)]);
        for _ in 0..CLAIM_MISSES + 2 {
            assert!(expire_claims(&mut held, &data, |_| false).is_empty());
        }
        assert!(held.contains_key("bd-1"));
    }

    #[test]
    fn claim_is_held_while_its_agent_is_alive() {
        let data = DashboardData::new(vec![issue("bd-1", "open", None)]);
        let mut held = claims([("bd-1", true)]);
        for _ in 0..CLAIM_MISSES + 2 {
            assert!(expire_claims(&mut held, &data, |_| true).is_empty());
        }
        assert!(held.contains_key("bd-1"));
    }

    #[test]
    fn claim_expires_once_its_agent_is_gone_for_good() {
        let data = DashboardData::new(vec![issue("bd-1", "open", None)]);
        let mut held = claims([("bd-1", true)]);
        for _ in 1..CLAIM_MISSES {
            assert!(expire_claims(&mut held, &data, |_| false).is_empty());
        }
        let expired = expire_claims(&mut held, &data, |_| false);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, "bd-1");
        // The bead it was holding comes back so the caller can requeue it.
        assert_eq!(expired[0].1.entry.as_ref().map(|entry| entry.id.as_str()), Some("bd-1"));
        assert!(held.is_empty());
    }

    #[test]
    fn a_scan_that_sees_the_agent_again_resets_the_count() {
        let data = DashboardData::new(vec![issue("bd-1", "open", None)]);
        let mut held = claims([("bd-1", true)]);
        for _ in 1..CLAIM_MISSES {
            expire_claims(&mut held, &data, |_| false);
        }
        assert!(expire_claims(&mut held, &data, |_| true).is_empty());
        for _ in 1..CLAIM_MISSES {
            assert!(expire_claims(&mut held, &data, |_| false).is_empty());
        }
        assert!(held.contains_key("bd-1"));
    }

    #[test]
    fn a_confirmed_claim_is_dropped_not_expired() {
        let data = DashboardData::new(vec![issue("bd-1", "in_progress", None)]);
        let mut held = claims([("bd-1", true)]);
        assert!(expire_claims(&mut held, &data, |_| false).is_empty());
        assert!(held.is_empty());
    }

    #[test]
    fn a_claim_on_a_bead_bd_has_not_reported_is_kept() {
        let data = DashboardData::new(Vec::new());
        let mut held = claims([("mystery", false)]);
        assert!(expire_claims(&mut held, &data, |_| false).is_empty());
        assert!(held.contains_key("mystery"));
    }

    #[test]
    fn next_ready_skips_blocked_beads() {
        let data = DashboardData::new(vec![
            issue("held", "open", Some("busy")),
            issue("busy", "in_progress", None),
            issue("free", "open", None),
        ]);
        let queue = vec![entry("held"), entry("free")];
        assert_eq!(next_ready(&queue, &data).map(|entry| entry.id.as_str()), Some("free"));
    }
}
