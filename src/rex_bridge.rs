use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::discovery::home_directory;

const OVERLAY_LABELS: &[&str] = &[
    "mdpeek-float",
    "mdpeek-dock",
    "omnidiff-float",
    "omnidiff-dock",
    "lazygit-float",
    "scratch-float",
    "btop-float",
];

#[derive(Debug, Clone, Default)]
pub struct RexSessionContext {
    pub session_id: Option<String>,
    pub self_block_id: Option<String>,
}

pub fn resolve_rex_binary() -> String {
    if let Ok(explicit) = env::var("REX_BIN")
        && !explicit.is_empty()
        && Path::new(&explicit).is_file()
    {
        return explicit;
    }

    let exe_name = format!("rex{}", env::consts::EXE_SUFFIX);
    let home = home_directory();
    for rel_dir in [".local/bin", ".cargo/bin"] {
        let candidate = home.join(rel_dir).join(&exe_name);
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }

    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        let base = PathBuf::from(local_app_data);
        for rel in ["Rex/rex.exe", "Programs/Rex/rex.exe"] {
            let candidate = base.join(rel);
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }

    if let Some(program_files) = env::var_os("ProgramFiles") {
        let candidate = PathBuf::from(program_files).join("Rex").join("rex.exe");
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }

    for candidate in [
        "/opt/homebrew/bin/rex",
        "/usr/local/bin/rex",
        "/usr/bin/rex",
        "/Applications/Rex.app/Contents/Helpers/rex",
        "/Applications/Rex Beta.app/Contents/Helpers/rex",
    ] {
        if Path::new(candidate).is_file() {
            return candidate.to_string();
        }
    }

    "rex".to_string()
}

fn execute_rex_json(args: &[&str]) -> Option<Value> {
    let output = Command::new(resolve_rex_binary()).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice::<Value>(&output.stdout).ok()
}

pub fn detect_session_context(explicit_session_id: Option<String>) -> RexSessionContext {
    let mut session_id = explicit_session_id
        .filter(|s| !s.is_empty())
        .or_else(|| env::var("REX_SESSION").ok().filter(|s| !s.is_empty()));
    let mut self_block_id = env::var("REX_BLOCK").ok().filter(|s| !s.is_empty());

    if session_id.is_none()
        && let Some(who) = execute_rex_json(&["whoami", "--json"])
    {
        session_id = who
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        if self_block_id.is_none() {
            self_block_id = who
                .get("block_id")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }

    RexSessionContext {
        session_id,
        self_block_id,
    }
}

pub fn extract_path_from_file_url(url: &str) -> Option<String> {
    let after_scheme = url.strip_prefix("file://")?;
    let slash_index = after_scheme.find('/')?;
    let raw_path = &after_scheme[slash_index..];
    let bytes = raw_path.as_bytes();
    if bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
    {
        return Some(raw_path[1..].to_string());
    }
    (!raw_path.is_empty()).then(|| raw_path.to_string())
}

pub fn query_block_working_directory(session_id: &str, block_id: &str) -> Option<PathBuf> {
    if let Some(process_info) = execute_rex_json(&[
        "block", "call", "-s", session_id, "-b", block_id, "process", "{}",
    ]) {
        if let Some(cwd) = process_info
            .pointer("/foreground/cwd")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return Some(PathBuf::from(cwd));
        }
        if let Some(ancestors) = process_info.get("ancestors").and_then(Value::as_array) {
            for ancestor in ancestors {
                if let Some(cwd) = ancestor
                    .get("cwd")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    return Some(PathBuf::from(cwd));
                }
            }
        }
    }

    if let Some(pwd_info) = execute_rex_json(&[
        "block", "call", "-s", session_id, "-b", block_id, "pwd", "{}",
    ]) && let Some(pwd_url) = pwd_info.get("pwd").and_then(Value::as_str)
        && let Some(parsed) = extract_path_from_file_url(pwd_url)
    {
        return Some(PathBuf::from(parsed));
    }

    None
}

pub fn resolve_live_chat_block(
    session_id: &str,
    self_block_id: Option<&str>,
    preferred_source_block: Option<&str>,
) -> Option<(String, Option<PathBuf>)> {
    let inspect = execute_rex_json(&["session", "inspect", session_id, "--json"])?;
    let active_window_id = inspect.get("active_window_id").and_then(Value::as_str);
    let windows = inspect.get("windows").and_then(Value::as_array)?;

    let window = windows
        .iter()
        .find(|w| w.get("window_id").and_then(Value::as_str) == active_window_id)
        .or_else(|| windows.first())?;

    let mut non_overlay_blocks: Vec<String> = Vec::new();
    if let Some(layers) = window.get("layers").and_then(Value::as_array) {
        for layer in layers {
            if let Some(blocks) = layer.get("blocks").and_then(Value::as_array) {
                for block in blocks {
                    let label = block.get("label").and_then(Value::as_str).unwrap_or("");
                    if OVERLAY_LABELS.contains(&label) {
                        continue;
                    }
                    if let Some(bid) = block.get("block_id").and_then(Value::as_str)
                        && Some(bid) != self_block_id
                        && !non_overlay_blocks.iter().any(|existing| existing == bid)
                    {
                        non_overlay_blocks.push(bid.to_string());
                    }
                }
            }
        }
    }

    let focused_id = window.get("focused_block_id").and_then(Value::as_str);
    let selected_block = focused_id
        .filter(|fid| Some(*fid) != self_block_id && non_overlay_blocks.iter().any(|b| b == *fid))
        .map(str::to_string)
        .or_else(|| {
            preferred_source_block
                .filter(|src| {
                    Some(*src) != self_block_id && non_overlay_blocks.iter().any(|b| b == *src)
                })
                .map(str::to_string)
        })
        .or_else(|| non_overlay_blocks.into_iter().next())?;

    let cwd = query_block_working_directory(session_id, &selected_block);
    Some((selected_block, cwd))
}

pub fn capture_terminal_scrollback(session_id: &str, block_id: &str) -> Option<String> {
    let output = Command::new(resolve_rex_binary())
        .args(["capture", "-s", session_id, "-b", block_id, "--trim"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn close_self_in_rex(session_id: Option<&str>, self_block_id: Option<&str>, mode: &str) {
    let Some(sid) = session_id else { return };
    let Some(bid) = self_block_id else { return };
    let rex_bin = resolve_rex_binary();

    if mode == "float"
        && let Some(inspect) = execute_rex_json(&["session", "inspect", sid, "--json"])
        && let Some(windows) = inspect.get("windows").and_then(Value::as_array)
    {
        for window in windows {
            if let Some(layers) = window.get("layers").and_then(Value::as_array) {
                for layer in layers {
                    if layer.get("kind").and_then(Value::as_str) == Some("tiled") {
                        continue;
                    }
                    let contains_self = layer
                        .get("blocks")
                        .and_then(Value::as_array)
                        .is_some_and(|blocks| {
                            blocks
                                .iter()
                                .any(|b| b.get("block_id").and_then(Value::as_str) == Some(bid))
                        });
                    if contains_self
                        && let Some(layer_id) = layer.get("layer_id").and_then(Value::as_str)
                    {
                        let payload = serde_json::json!({ "layer_id": layer_id }).to_string();
                        let _ = Command::new(&rex_bin)
                            .args(["api", "call", "-s", sid, "session.close_layer", &payload])
                            .output();
                        return;
                    }
                }
            }
        }
    }

    let _ = Command::new(&rex_bin)
        .args(["close", "-s", sid, bid])
        .output();
}
