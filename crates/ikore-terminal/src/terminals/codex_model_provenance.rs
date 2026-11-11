use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};
use serde_json::Value;
use uuid::Uuid;

use super::MAX_MODEL_CHARS;

/// Codex's public `exec --json` item events omit the model. Its own rollout
/// records the effective model in `turn_context` for each started turn.
pub(super) struct CodexModelEvidence {
    pub latest_model: String,
    pub turn_models: Vec<String>,
}

pub(super) fn current_session_model(
    session_id: &str,
    run_created_at: &str,
) -> Option<CodexModelEvidence> {
    let home = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME"))?;
    read_session_model(
        &PathBuf::from(home).join(".codex/sessions"),
        session_id,
        run_created_at,
    )
}

pub(super) fn session_model_from_root(
    sessions_root: &Path,
    session_id: &str,
    run_created_at: &str,
) -> Option<CodexModelEvidence> {
    read_session_model(sessions_root, session_id, run_created_at)
}

fn read_session_model(
    sessions_root: &Path,
    session_id: &str,
    run_created_at: &str,
) -> Option<CodexModelEvidence> {
    let session_id = Uuid::parse_str(session_id).ok()?.to_string();
    let run_created_at = DateTime::parse_from_rfc3339(run_created_at).ok()?;
    let suffix = format!("-{session_id}.jsonl");
    let path = walkdir::WalkDir::new(sessions_root)
        .max_depth(4)
        .into_iter()
        .filter_map(Result::ok)
        .find(|entry| {
            entry.file_type().is_file()
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(&suffix))
        })?
        .into_path();
    parse_rollout(&path, &session_id, run_created_at)
}

fn parse_rollout(
    path: &Path,
    session_id: &str,
    run_created_at: DateTime<FixedOffset>,
) -> Option<CodexModelEvidence> {
    let file = File::open(path).ok()?;
    let mut matched_session = false;
    let mut started_turns = HashSet::new();
    let mut turn_models = HashMap::new();
    let mut ordered_models = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line.ok()?;
        if !line.contains("\"session_meta\"")
            && !line.contains("\"task_started\"")
            && !line.contains("\"turn_context\"")
        {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            // A concurrently written final line is incomplete. Try again on
            // the next bounded probe instead of trusting an earlier turn.
            return None;
        };
        let payload = value.get("payload")?;
        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                matched_session = payload.get("id").and_then(Value::as_str) == Some(session_id)
                    || payload.get("session_id").and_then(Value::as_str) == Some(session_id);
            }
            Some("event_msg")
                if payload.get("type").and_then(Value::as_str) == Some("task_started") =>
            {
                let timestamp = value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(|value| DateTime::parse_from_rfc3339(value).ok());
                if timestamp.is_some_and(|timestamp| timestamp >= run_created_at)
                    && let Some(turn_id) = payload.get("turn_id").and_then(Value::as_str)
                {
                    started_turns.insert(turn_id.to_owned());
                }
            }
            Some("turn_context") => {
                let Some(turn_id) = payload.get("turn_id").and_then(Value::as_str) else {
                    continue;
                };
                if !started_turns.contains(turn_id) {
                    continue;
                }
                let model = payload
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|value| {
                        !value.trim().is_empty()
                            && value.len() <= MAX_MODEL_CHARS
                            && !value.chars().any(char::is_control)
                    })?
                    .to_owned();
                turn_models.insert(turn_id.to_owned(), model.clone());
                ordered_models.push(model);
            }
            _ => {}
        }
    }
    if !matched_session
        || started_turns.is_empty()
        || started_turns
            .iter()
            .any(|turn_id| !turn_models.contains_key(turn_id))
    {
        return None;
    }
    Some(CodexModelEvidence {
        latest_model: ordered_models.last()?.clone(),
        turn_models: ordered_models,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_rollout_proves_only_current_session_turn_models() {
        let root = tempfile::tempdir().unwrap();
        let day = root.path().join("2026/09/23");
        std::fs::create_dir_all(&day).unwrap();
        let session_id = "01a0d083-14a1-77c1-9604-b2f1493ea83f";
        let path = day.join(format!("rollout-2026-09-23T20-04-06-{session_id}.jsonl"));
        let lines = [
            serde_json::json!({"timestamp":"2026-09-23T22:00:00Z","type":"session_meta","payload":{"id":session_id}}),
            serde_json::json!({"timestamp":"2026-09-23T22:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"old"}}),
            serde_json::json!({"timestamp":"2026-09-23T22:00:02Z","type":"turn_context","payload":{"turn_id":"old","model":"gpt-5.4"}}),
            serde_json::json!({"timestamp":"2026-09-23T23:04:06Z","type":"event_msg","payload":{"type":"task_started","turn_id":"new"}}),
            serde_json::json!({"timestamp":"2026-09-23T23:04:07Z","type":"turn_context","payload":{"turn_id":"new","model":"gpt-5.6-luna"}}),
        ];
        std::fs::write(
            &path,
            lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let evidence = read_session_model(
            root.path(),
            session_id,
            "2026-09-23T23:04:05.127814900+00:00",
        )
        .unwrap();
        assert_eq!(evidence.latest_model, "gpt-5.6-luna");
        assert_eq!(evidence.turn_models, ["gpt-5.6-luna"]);
        assert!(read_session_model(root.path(), "../different", "2026-09-23T23:04:05Z").is_none());
        assert!(read_session_model(root.path(), session_id, "2026-09-23T23:05:00Z").is_none());

        let mut changed = lines.to_vec();
        changed.push(serde_json::json!({"timestamp":"2026-09-23T23:05:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"changed"}}));
        changed.push(serde_json::json!({"timestamp":"2026-09-23T23:05:02Z","type":"turn_context","payload":{"turn_id":"changed","model":"gpt-5.6-sol"}}));
        std::fs::write(
            &path,
            changed
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let changed = read_session_model(root.path(), session_id, "2026-09-23T23:04:05Z").unwrap();
        assert_eq!(changed.turn_models, ["gpt-5.6-luna", "gpt-5.6-sol"]);

        std::fs::write(
            &path,
            lines[..4]
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        assert!(read_session_model(root.path(), session_id, "2026-09-23T23:04:05Z").is_none());
    }
}
