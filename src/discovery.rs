use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryScope {
    Chat,
    Diagrams,
    Workspace,
}

impl DiscoveryScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Diagrams => "Diagrams",
            Self::Workspace => "Workspace",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredMarkdownFile {
    pub path: PathBuf,
    pub display_name: String,
    pub parent_hint: String,
    pub scope: DiscoveryScope,
    pub modified_epoch_secs: u64,
}

pub fn normalize_canonical_path(path: &Path) -> PathBuf {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    {
        if let Some(raw) = canonical.to_str()
            && let Some(stripped) = raw.strip_prefix(r"\\?\")
        {
            return PathBuf::from(stripped);
        }
    }
    canonical
}

fn read_modified_epoch_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn build_discovered_entry(
    path: PathBuf,
    scope: DiscoveryScope,
    workspace_cwd: &Path,
    home_dir: &Path,
) -> Option<DiscoveredMarkdownFile> {
    let canonical = normalize_canonical_path(&path);
    if !canonical.is_file() {
        return None;
    }
    let display_name = canonical
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("document.md")
        .to_string();

    let parent_dir = canonical.parent().unwrap_or_else(|| Path::new("."));
    let parent_hint = if let Ok(rel) = parent_dir.strip_prefix(workspace_cwd) {
        let rel_str = rel.to_string_lossy();
        if rel_str.is_empty() {
            ".".to_string()
        } else {
            format!("./{rel_str}")
        }
    } else if let Ok(rel_home) = parent_dir.strip_prefix(home_dir) {
        format!("~/{}", rel_home.to_string_lossy())
    } else {
        parent_dir.to_string_lossy().into_owned()
    };

    Some(DiscoveredMarkdownFile {
        modified_epoch_secs: read_modified_epoch_secs(&canonical),
        path: canonical,
        display_name,
        parent_hint,
        scope,
    })
}

pub fn percent_decode_path(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'%'
            && idx + 2 < bytes.len()
            && let Ok(hex_str) = std::str::from_utf8(&bytes[idx + 1..idx + 3])
            && let Ok(value) = u8::from_str_radix(hex_str, 16)
        {
            decoded.push(value);
            idx += 3;
            continue;
        }
        decoded.push(bytes[idx]);
        idx += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn strip_tui_box_chrome(raw_line: &str) -> &str {
    let mut trimmed = raw_line.trim_end();
    for border in ['│', '┃', '║', '▏', '▕'] {
        trimmed = trimmed.trim_start_matches(border).trim_end_matches(border);
    }
    trimmed
}

fn contains_windows_drive_prefix(text: &str) -> bool {
    text.as_bytes()
        .windows(3)
        .any(|w| w[0].is_ascii_alphabetic() && w[1] == b':' && (w[2] == b'/' || w[2] == b'\\'))
}

fn is_windows_absolute_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\'))
        || text.starts_with(r"\\")
}

fn join_soft_wrapped_paths(capture_text: &str) -> Vec<String> {
    let cleaned_lines: Vec<String> = capture_text
        .lines()
        .map(|l| strip_tui_box_chrome(l).to_string())
        .collect();

    let mut logical_lines: Vec<String> = Vec::with_capacity(cleaned_lines.len());
    for line in cleaned_lines {
        if let Some(prev) = logical_lines.last_mut() {
            let prev_trimmed = prev.trim_end();
            let curr_trimmed = line.trim_start();
            let prev_looks_mid_path = (prev_trimmed.contains("file://")
                || prev_trimmed.contains("/Users/")
                || prev_trimmed.contains("/home/")
                || prev_trimmed.contains("/tmp/")
                || prev_trimmed.contains("/var/")
                || prev_trimmed.contains("~/")
                || prev_trimmed.contains(r"~\")
                || contains_windows_drive_prefix(prev_trimmed))
                && !prev_trimmed.ends_with(')')
                && !prev_trimmed.ends_with('`')
                && !prev_trimmed.ends_with('"')
                && !prev_trimmed.to_ascii_lowercase().ends_with(".md");
            let curr_lower = curr_trimmed.to_ascii_lowercase();
            let curr_continues_path = !curr_trimmed.is_empty()
                && !curr_trimmed.contains(' ')
                && (curr_lower.ends_with(".md")
                    || curr_lower.contains(".md)")
                    || curr_lower.contains(".md#")
                    || curr_trimmed.contains('/')
                    || curr_trimmed.contains('\\'));

            if prev_looks_mid_path && curr_continues_path {
                prev.push_str(curr_trimmed);
                continue;
            }
        }
        logical_lines.push(line);
    }
    logical_lines
}

fn extract_candidate_tokens(line: &str) -> Vec<String> {
    let delimiters = [
        ' ', '\t', '(', ')', '[', ']', '<', '>', '"', '\'', '`', ',', ';', '*',
    ];
    line.split(|c: char| delimiters.contains(&c))
        .map(str::trim)
        .filter(|tok| !tok.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn resolve_markdown_token(token: &str, base_cwd: &Path, home_dir: &Path) -> Option<PathBuf> {
    let without_fragment = token.split('#').next().unwrap_or(token);
    let without_query = without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment)
        .trim_end_matches(['.', ':', ';']);

    if !without_query.to_ascii_lowercase().ends_with(".md") {
        return None;
    }

    if let Some(after_scheme) = without_query.strip_prefix("file://") {
        let slash_pos = after_scheme.find('/')?;
        let decoded = percent_decode_path(&after_scheme[slash_pos..]);
        let bytes = decoded.as_bytes();
        if bytes.len() >= 3
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b':'
        {
            return Some(PathBuf::from(&decoded[1..]));
        }
        return Some(PathBuf::from(decoded));
    }

    if let Some(rel_home) = without_query
        .strip_prefix("~/")
        .or_else(|| without_query.strip_prefix(r"~\"))
    {
        return Some(home_dir.join(percent_decode_path(rel_home)));
    }

    let decoded = percent_decode_path(without_query);
    if decoded.starts_with('/') || is_windows_absolute_path(&decoded) {
        return Some(PathBuf::from(decoded));
    }

    if !decoded.is_empty() {
        return Some(base_cwd.join(decoded));
    }

    None
}

pub fn extract_markdown_files_from_chat(
    capture_text: &str,
    workspace_cwd: &Path,
    home_dir: &Path,
) -> Vec<DiscoveredMarkdownFile> {
    let normalized_cwd = normalize_canonical_path(workspace_cwd);
    let normalized_home = normalize_canonical_path(home_dir);
    let mut seen = HashSet::new();
    let mut results = Vec::new();
    let logical_lines = join_soft_wrapped_paths(capture_text);

    for line in logical_lines.iter().rev() {
        let mut line_candidates = Vec::new();
        for token in extract_candidate_tokens(line) {
            if let Some(candidate_path) =
                resolve_markdown_token(&token, &normalized_cwd, &normalized_home)
                && let Some(entry) = build_discovered_entry(
                    candidate_path,
                    DiscoveryScope::Chat,
                    &normalized_cwd,
                    &normalized_home,
                )
            {
                line_candidates.push(entry);
            }
        }
        for entry in line_candidates.into_iter().rev() {
            if seen.insert(entry.path.clone()) {
                results.push(entry);
            }
        }
    }

    results
}

fn diagrams_search_roots() -> Vec<PathBuf> {
    let mut roots = vec![env::temp_dir().join("diagrams")];
    #[cfg(unix)]
    {
        roots.push(PathBuf::from("/tmp/diagrams"));
        roots.push(PathBuf::from("/private/tmp/diagrams"));
    }
    roots
}

pub fn scan_diagrams_directory(
    workspace_cwd: &Path,
    home_dir: &Path,
) -> Vec<DiscoveredMarkdownFile> {
    let normalized_cwd = normalize_canonical_path(workspace_cwd);
    let normalized_home = normalize_canonical_path(home_dir);
    let mut seen = HashSet::new();
    let mut files = Vec::new();

    for root in diagrams_search_roots() {
        if !root.is_dir() {
            continue;
        }
        collect_markdown_recursive(
            &root,
            0,
            2,
            DiscoveryScope::Diagrams,
            &normalized_cwd,
            &normalized_home,
            &mut seen,
            &mut files,
        );
    }

    files.sort_by(|a, b| {
        b.modified_epoch_secs
            .cmp(&a.modified_epoch_secs)
            .then_with(|| a.display_name.cmp(&b.display_name))
    });
    files
}

pub fn scan_workspace_markdown_files(
    workspace_cwd: &Path,
    home_dir: &Path,
) -> Vec<DiscoveredMarkdownFile> {
    let normalized_cwd = normalize_canonical_path(workspace_cwd);
    let normalized_home = normalize_canonical_path(home_dir);
    let max_depth = if normalized_cwd == normalized_home || normalized_cwd.parent().is_none() {
        0
    } else {
        3
    };

    let mut seen = HashSet::new();
    let mut files = Vec::new();
    collect_markdown_recursive(
        &normalized_cwd,
        0,
        max_depth,
        DiscoveryScope::Workspace,
        &normalized_cwd,
        &normalized_home,
        &mut seen,
        &mut files,
    );
    files.sort_by(|a, b| {
        b.modified_epoch_secs
            .cmp(&a.modified_epoch_secs)
            .then_with(|| a.display_name.cmp(&b.display_name))
    });
    files.truncate(80);
    files
}

#[allow(clippy::too_many_arguments)]
fn collect_markdown_recursive(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    scope: DiscoveryScope,
    workspace_cwd: &Path,
    home_dir: &Path,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<DiscoveredMarkdownFile>,
) {
    if depth > max_depth {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with('.')
            || matches!(
                name,
                "node_modules"
                    | "target"
                    | "dist"
                    | "build"
                    | "vendor"
                    | "__pycache__"
                    | "Library"
                    | "AppData"
                    | "Applications"
            )
        {
            continue;
        }
        if path.is_dir() {
            collect_markdown_recursive(
                &path,
                depth + 1,
                max_depth,
                scope,
                workspace_cwd,
                home_dir,
                seen,
                out,
            );
        } else if path
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
            && let Some(discovered) = build_discovered_entry(path, scope, workspace_cwd, home_dir)
            && seen.insert(discovered.path.clone())
        {
            out.push(discovered);
        }
    }
}

pub fn home_directory() -> PathBuf {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .or_else(|| {
            let drive = env::var_os("HOMEDRIVE")?;
            let path = env::var_os("HOMEPATH")?;
            let mut combined = PathBuf::from(drive);
            combined.push(path);
            Some(combined)
        })
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn extracts_latest_markdown_links_and_soft_wrapped_paths_from_chat() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let first_md = tmp.path().join("older-notes.md");
        let second_md = tmp.path().join("auth-sequence-diagram.md");
        File::create(&first_md)
            .unwrap()
            .write_all(b"# Older\n")
            .unwrap();
        File::create(&second_md)
            .unwrap()
            .write_all(b"# Auth\n")
            .unwrap();

        let second_str = second_md.to_string_lossy();
        let split_at = second_str.len() - 10;
        let part_a = &second_str[..split_at];
        let part_b = &second_str[split_at..];

        let simulated_capture = format!(
            "│ Earlier doc: [older](file://{}) │\n│ Latest diagram: file://{}\n{} │\n",
            first_md.display(),
            part_a,
            part_b
        );

        let discovered =
            extract_markdown_files_from_chat(&simulated_capture, tmp.path(), tmp.path());
        assert_eq!(discovered.len(), 2);
        assert_eq!(discovered[0].path, normalize_canonical_path(&second_md));
        assert_eq!(discovered[1].path, normalize_canonical_path(&first_md));
    }

    #[test]
    fn resolves_windows_and_unix_markdown_tokens_cross_platform() {
        let cwd = Path::new("/workspace/repo");
        let home = Path::new("/home/alice");

        assert_eq!(
            resolve_markdown_token("file:///C:/Users/alice/My%20Docs/arch.md#L10", cwd, home),
            Some(PathBuf::from("C:/Users/alice/My Docs/arch.md"))
        );
        assert_eq!(
            resolve_markdown_token(r"C:\Users\alice\diagrams\flow.md:", cwd, home),
            Some(PathBuf::from(r"C:\Users\alice\diagrams\flow.md"))
        );
        assert_eq!(
            resolve_markdown_token(r"~\notes\design.md", cwd, home),
            Some(home.join(r"notes\design.md"))
        );
        assert_eq!(
            resolve_markdown_token("file:///tmp/diagrams/pele.md", cwd, home),
            Some(PathBuf::from("/tmp/diagrams/pele.md"))
        );
    }
}
