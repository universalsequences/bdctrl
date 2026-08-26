use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde_json::Value;

use crate::herdr::AgentInfo;

// Claude Code writes one JSONL transcript per session under
// ~/.claude/projects/<slug>/. A session we launched opens with the
// "Take on bead <id>" prompt, and every assistant line records the token
// usage of that API call — so matching transcript heads to beads and reading
// the newest usage from the tail gives the agent's live context size without
// talking to the agent at all.

const HEAD_BYTES: u64 = 256 * 1024;
const TAIL_BYTES: u64 = 256 * 1024;

#[derive(Default)]
pub struct SessionIndex {
    // Bead id from each transcript's first user message. Heads never change,
    // so each file is read at most once.
    heads: HashMap<PathBuf, Option<String>>,
    // Bead each transcript currently holds a claim on, plus how far into the
    // file that answer was read from.
    claims: HashMap<PathBuf, ClaimScan>,
}

#[derive(Default)]
struct ClaimScan {
    scanned: u64,
    bead: Option<String>,
}

enum Claim {
    Take(String),
    Drop(String),
}

impl SessionIndex {
    fn bead_for(&mut self, path: &Path, modified: SystemTime) -> Option<String> {
        if let Some(cached) = self.heads.get(path) {
            return cached.clone();
        }
        let text = read_head(path)?;
        let (bead, saw_user) = bead_from_head(&text);
        // A fresh session may not have its prompt on disk yet; only cache a
        // miss once a user message exists or the file has clearly settled.
        let settled =
            saw_user || modified.elapsed().is_ok_and(|age| age > Duration::from_secs(600));
        if bead.is_some() || settled {
            self.heads.insert(path.to_owned(), bead.clone());
        }
        bead
    }

    // The bead a session is holding: the last `bd update <id> --claim` it ran,
    // released again when that same bead is closed. Transcripts only ever grow,
    // so each pass reads just the lines appended since the last one — the
    // scan stays cheap enough to run on every refresh tick.
    pub fn claimed_bead(&mut self, path: &Path, known: &HashSet<&str>) -> Option<String> {
        let scan = self.claims.entry(path.to_owned()).or_default();
        let Ok(length) = fs::metadata(path).map(|metadata| metadata.len()) else {
            return scan.bead.clone();
        };
        // A rewritten (compacted) transcript is no longer the file we read.
        if length < scan.scanned {
            *scan = ClaimScan::default();
        }
        if length > scan.scanned
            && let Some((text, consumed)) = read_from(path, scan.scanned)
        {
            for line in text.lines() {
                for claim in claims_in_line(line, known) {
                    match claim {
                        Claim::Take(bead) => scan.bead = Some(bead),
                        Claim::Drop(bead) if scan.bead.as_deref() == Some(&bead) => {
                            scan.bead = None
                        }
                        Claim::Drop(_) => {}
                    }
                }
            }
            scan.scanned += consumed;
        }
        scan.bead.clone()
    }
}

// Claims recorded by one transcript line, in the order the agent ran them.
fn claims_in_line(line: &str, known: &HashSet<&str>) -> Vec<Claim> {
    if !line.contains("--claim") && !line.contains("bd close") && !line.contains("in_progress") {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    // Subagents run in their own context; a claim there is still the parent
    // session's work, but a sidechain never claims on its own behalf.
    if value["isSidechain"].as_bool() == Some(true) {
        return Vec::new();
    }
    value["message"]["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block["input"]["command"].as_str())
                .flat_map(|command| claims_in_command(command, known))
                .collect()
        })
        .unwrap_or_default()
}

// One shell command may chain several `bd` calls; each pipeline segment is
// checked on its own so `bd show x && bd update x --claim` is not read as a
// single invocation.
fn claims_in_command(command: &str, known: &HashSet<&str>) -> Vec<Claim> {
    let mut claims = Vec::new();
    for segment in command.split(['\n', ';', '|', '&']) {
        let mut tokens = segment.split_whitespace().skip_while(|token| *token != "bd");
        if tokens.next().is_none() {
            continue;
        }
        let Some(subcommand) = tokens.next() else {
            continue;
        };
        let arguments: Vec<&str> = tokens.collect();
        // The bead is whichever argument is an id we know; that beats
        // positional guessing, which flag values like `--status in_progress`
        // would otherwise break.
        let Some(bead) = arguments
            .iter()
            .map(|token| unquote(token))
            .find(|token| known.contains(token))
            .map(str::to_owned)
        else {
            continue;
        };
        let claimed = arguments
            .iter()
            .any(|token| *token == "--claim" || unquote(token) == "in_progress");
        match subcommand {
            "update" if claimed => claims.push(Claim::Take(bead)),
            "claim" => claims.push(Claim::Take(bead)),
            "close" => claims.push(Claim::Drop(bead)),
            _ => {}
        }
    }
    claims
}

fn unquote(token: &str) -> &str {
    token.trim_matches(|character| "\"'`".contains(character))
}

// Text appended since `offset`, truncated to whole lines, with the number of
// bytes those lines cover so the next pass resumes on a line boundary.
fn read_from(path: &Path, offset: u64) -> Option<(String, u64)> {
    let mut file = fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).ok()?;
    let end = buffer.iter().rposition(|byte| *byte == b'\n')? + 1;
    buffer.truncate(end);
    Some((String::from_utf8_lossy(&buffer).into_owned(), end as u64))
}

// Fill in context_tokens for every Claude agent that has a matching session
// transcript. Files are visited newest-first, so a relaunched bead reads its
// current session rather than an abandoned one.
pub fn attach_context(
    agents: &mut HashMap<String, AgentInfo>,
    project: &Path,
    index: &mut SessionIndex,
) {
    if !agents.values().any(|agent| agent.kind.contains("claude")) {
        return;
    }
    let Some(dir) = transcript_dir(project) else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "jsonl") {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    files.sort_by(|left, right| right.0.cmp(&left.0));

    for (modified, path) in &files {
        let Some(bead) = index.bead_for(path, *modified) else {
            continue;
        };
        let Some(agent) = agents.get_mut(&bead) else {
            continue;
        };
        if agent.kind.contains("claude") && agent.context_tokens.is_none() {
            agent.context_tokens = context_tokens(path);
        }
    }
}

// Where Claude Code keeps the transcripts for sessions started in `project`.
pub fn transcript_dir(project: &Path) -> Option<PathBuf> {
    let home = env::var_os("HOME")?;
    let slug: String = project
        .display()
        .to_string()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    Some(PathBuf::from(home).join(".claude/projects").join(slug))
}

// (bead id, whether a first user message was present at all)
fn bead_from_head(text: &str) -> (Option<String>, bool) {
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value["type"].as_str() != Some("user")
            || value["isSidechain"].as_bool() == Some(true)
        {
            continue;
        }
        let content = &value["message"]["content"];
        let prompt = match content.as_str() {
            Some(text) => text.to_owned(),
            None => content
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|block| block["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),
        };
        let marker = "Take on bead ";
        let bead = prompt.find(marker).and_then(|start| {
            let rest = &prompt[start + marker.len()..];
            let id = rest[..rest.find(':')?].trim();
            (!id.is_empty() && id.len() < 128).then(|| id.to_owned())
        });
        return (bead, true);
    }
    (None, false)
}

pub fn context_tokens(path: &Path) -> Option<u64> {
    read_tail(path).and_then(|text| tokens_from_tail(&text))
}

// Context size after the latest main-chain assistant turn: everything on the
// input side of that API call, cached or not.
fn tokens_from_tail(text: &str) -> Option<u64> {
    for line in text.lines().rev() {
        if !line.contains("\"usage\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value["type"].as_str() != Some("assistant")
            || value["isSidechain"].as_bool() == Some(true)
        {
            continue;
        }
        let usage = &value["message"]["usage"];
        if usage["input_tokens"].is_u64() || usage["cache_read_input_tokens"].is_u64() {
            return Some(
                usage["input_tokens"].as_u64().unwrap_or(0)
                    + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                    + usage["cache_read_input_tokens"].as_u64().unwrap_or(0),
            );
        }
    }
    None
}

fn read_head(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut buffer = vec![0u8; HEAD_BYTES as usize];
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(_) => return None,
        }
    }
    buffer.truncate(filled);
    Some(String::from_utf8_lossy(&buffer).into_owned())
}

fn read_tail(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).ok()?;
    let mut text = String::from_utf8_lossy(&buffer).into_owned();
    // A mid-file seek almost certainly landed inside a line; drop the partial.
    if start > 0 {
        if let Some(newline) = text.find('\n') {
            text.drain(..=newline);
        }
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_finds_bead_in_first_user_message() {
        let head = concat!(
            r#"{"type":"mode","mode":"default"}"#,
            "\n",
            r#"{"type":"file-history-snapshot","snapshot":{}}"#,
            "\n",
            r#"{"type":"user","isSidechain":false,"message":{"role":"user","content":"Take on bead bd-42: Fix the flux capacitor. Start by running `bd show bd-42`."}}"#,
            "\n",
        );
        assert_eq!(bead_from_head(head), (Some("bd-42".into()), true));

        let blocks = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Take on bead x-1: Do it."}]}}"#;
        assert_eq!(bead_from_head(blocks), (Some("x-1".into()), true));

        let unrelated = r#"{"type":"user","message":{"role":"user","content":"hello"}}"#;
        assert_eq!(bead_from_head(unrelated), (None, true));
        assert_eq!(bead_from_head(r#"{"type":"mode"}"#), (None, false));
    }

    #[test]
    fn tail_reads_latest_main_chain_usage() {
        let tail = concat!(
            r#"{"type":"assistant","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":100,"cache_creation_input_tokens":5}}}"#,
            "\n",
            r#"{"type":"assistant","isSidechain":true,"message":{"usage":{"input_tokens":999999}}}"#,
            "\n",
            r#"{"type":"assistant","message":{"usage":{"input_tokens":2,"cache_read_input_tokens":325696,"cache_creation_input_tokens":702,"output_tokens":421}}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"ok"}}"#,
            "\n",
        );
        assert_eq!(tokens_from_tail(tail), Some(2 + 325696 + 702));
        assert_eq!(tokens_from_tail("{}\n"), None);
    }

    #[test]
    fn reads_claims_and_releases_out_of_bd_commands() {
        let known = HashSet::from(["bd-42", "bd-9", "bd-9.1"]);
        let claims = |command| {
            claims_in_command(command, &known)
                .into_iter()
                .map(|claim| match claim {
                    Claim::Take(bead) => format!("take {bead}"),
                    Claim::Drop(bead) => format!("drop {bead}"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(claims("bd update bd-42 --claim 2>&1"), ["take bd-42"]);
        assert_eq!(claims("bd update --status in_progress bd-9.1"), ["take bd-9.1"]);
        assert_eq!(claims("bd close bd-42 --reason=\"done\""), ["drop bd-42"]);
        // Each segment of a chain is its own invocation.
        assert_eq!(
            claims("cd /x && bd show bd-9; bd update bd-9 --claim && bd close bd-42"),
            ["take bd-9", "drop bd-42"]
        );
        // Beads we do not know, and commands that are not claims, say nothing.
        assert!(claims("bd update bd-77 --claim").is_empty());
        assert!(claims("bd update bd-42 --notes=\"in_progress somewhere\"").is_empty());
        assert!(claims("grep -rn 'bd update bd-42' src").is_empty());
    }

    #[test]
    fn tracks_the_bead_a_transcript_currently_holds() {
        let known = HashSet::from(["bd-42", "bd-9"]);
        let mut index = SessionIndex::default();
        let path = std::env::temp_dir().join("beadsctrl-claim-scan.jsonl");
        let entry = |command: &str| {
            format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","input":{{"command":"{command}"}}}}]}}}}"#
            )
        };

        fs::write(&path, entry("bd update bd-42 --claim") + "\n").unwrap();
        assert_eq!(index.claimed_bead(&path, &known).as_deref(), Some("bd-42"));

        // Only the bytes appended since the last pass are read.
        let mut appended = fs::read_to_string(&path).unwrap();
        appended.push_str(&(entry("bd close bd-42 --reason=done") + "\n"));
        appended.push_str(&(entry("bd update bd-9 --claim") + "\n"));
        fs::write(&path, appended).unwrap();
        assert_eq!(index.claimed_bead(&path, &known).as_deref(), Some("bd-9"));

        // A partial trailing line is left for the next pass.
        let held = fs::read_to_string(&path).unwrap() + &entry("bd close bd-9");
        fs::write(&path, held).unwrap();
        assert_eq!(index.claimed_bead(&path, &known).as_deref(), Some("bd-9"));
        fs::write(&path, fs::read_to_string(&path).unwrap() + "\n").unwrap();
        assert_eq!(index.claimed_bead(&path, &known), None);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn a_sidechain_claim_is_not_the_session_claim() {
        let known = HashSet::from(["bd-42"]);
        let line = r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"tool_use","input":{"command":"bd update bd-42 --claim"}}]}}"#;
        assert!(claims_in_line(line, &known).is_empty());
    }

    #[test]
    fn sessions_dir_slugifies_the_project_path() {
        let dir = transcript_dir(Path::new("/home/alec/code/bdctrl")).unwrap();
        assert!(dir.ends_with(".claude/projects/-home-alec-code-bdctrl"));
    }
}
