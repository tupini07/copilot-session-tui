use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Default)]
pub struct SessionDetails {
    pub edited_files: Vec<String>,
    pub last_user_message: Option<String>,
    pub turn_count: usize,
    pub tool_call_count: usize,
}

/// Parse events.jsonl to extract session details
pub fn parse_events(path: &Path) -> Result<SessionDetails> {
    let file = File::open(path)
        .with_context(|| format!("Failed to open {}", path.display()))?;
    let reader = BufReader::new(file);

    let mut edited_files_ordered: Vec<String> = Vec::new();
    let mut edited_files_seen = BTreeSet::new();
    let mut last_user_message = None;
    let mut turn_count = 0usize;
    let mut tool_call_count = 0usize;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        if line.is_empty() {
            continue;
        }

        let event: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = event["type"].as_str().unwrap_or("");

        match event_type {
            "user.message" => {
                turn_count += 1;
                if let Some(content) = event["data"]["content"].as_str() {
                    // Truncate long messages for preview
                    let preview = if content.len() > 200 {
                        format!("{}...", &content[..200])
                    } else {
                        content.to_string()
                    };
                    last_user_message = Some(preview);
                }
            }
            "tool.execution_start" => {
                tool_call_count += 1;
                let tool_name = event["data"]["toolName"].as_str().unwrap_or("");
                if matches!(tool_name, "edit" | "create") {
                    if let Some(path) = event["data"]["arguments"]["path"].as_str() {
                        let normalized = path.replace("\\\\", "\\");
                        if edited_files_seen.insert(normalized.clone()) {
                            edited_files_ordered.push(normalized);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(SessionDetails {
        edited_files: edited_files_ordered,
        last_user_message,
        turn_count,
        tool_call_count,
    })
}
