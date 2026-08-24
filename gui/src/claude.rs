use std::{
    collections::HashMap,
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
    let Some(dir) = sessions_dir(project) else {
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
            agent.context_tokens = read_tail(path).and_then(|text| tokens_from_tail(&text));
        }
    }
}

fn sessions_dir(project: &Path) -> Option<PathBuf> {
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
    fn sessions_dir_slugifies_the_project_path() {
        let dir = sessions_dir(Path::new("/home/alec/code/bdctrl")).unwrap();
        assert!(dir.ends_with(".claude/projects/-home-alec-code-bdctrl"));
    }
}
