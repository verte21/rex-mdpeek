use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::discovery::percent_decode_path;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub inline_code: bool,
    pub link_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledInlineSpan {
    pub text: String,
    pub style: InlineStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AlertKind {
    pub fn badge_label(self) -> &'static str {
        match self {
            Self::Note => "NOTE",
            Self::Tip => "TIP",
            Self::Important => "IMPORTANT",
            Self::Warning => "WARNING",
            Self::Caution => "CAUTION",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagramAssetSource {
    File(PathBuf),
    InlineMermaid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentBlock {
    Heading {
        level: u8,
        spans: Vec<StyledInlineSpan>,
    },
    Paragraph {
        spans: Vec<StyledInlineSpan>,
    },
    Alert {
        kind: AlertKind,
        paragraphs: Vec<Vec<StyledInlineSpan>>,
    },
    BlockQuote {
        paragraphs: Vec<Vec<StyledInlineSpan>>,
    },
    ListItem {
        depth: usize,
        marker: String,
        spans: Vec<StyledInlineSpan>,
    },
    CodeFence {
        language: String,
        lines: Vec<String>,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    DiagramVisual {
        alt_title: String,
        asset: DiagramAssetSource,
    },
    CollapsibleDetails {
        details_id: usize,
        summary: String,
        inner_blocks: Vec<DocumentBlock>,
    },
    ThematicBreak,
}

#[derive(Debug, Clone)]
pub struct ParsedMarkdownDocument {
    pub title: String,
    pub blocks: Vec<DocumentBlock>,
}

pub fn load_and_parse_markdown(path: &Path) -> Result<ParsedMarkdownDocument> {
    let raw_markdown = fs::read_to_string(path)
        .with_context(|| format!("failed to read Markdown file {}", path.display()))?;
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let fallback_title = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("Document");
    Ok(parse_markdown_source(&raw_markdown, base_dir, fallback_title))
}

pub fn parse_markdown_source(
    markdown_text: &str,
    document_dir: &Path,
    fallback_title: &str,
) -> ParsedMarkdownDocument {
    let mut parser_options = Options::empty();
    parser_options.insert(Options::ENABLE_TABLES);
    parser_options.insert(Options::ENABLE_STRIKETHROUGH);
    parser_options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(markdown_text, parser_options);

    let mut top_blocks: Vec<DocumentBlock> = Vec::new();
    let mut active_details: Option<(usize, String, Vec<DocumentBlock>)> = None;
    let mut next_details_id: usize = 1;
    let mut pending_summary_html = false;

    let mut style_stack: Vec<InlineStyle> = vec![InlineStyle::default()];
    let mut inline_buffer: Vec<StyledInlineSpan> = Vec::new();

    let mut current_heading_level: Option<u8> = None;
    let mut in_paragraph = false;
    let mut blockquote_depth: usize = 0;
    let mut blockquote_paragraphs: Vec<Vec<StyledInlineSpan>> = Vec::new();

    let mut list_stack: Vec<Option<u64>> = Vec::new();
    let mut pending_item_marker: Option<String> = None;

    let mut active_code_fence: Option<(String, String)> = None;
    let mut active_image: Option<(String, String)> = None;

    let mut in_table = false;
    let mut in_table_head = false;
    let mut table_headers: Vec<String> = Vec::new();
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_cell_text = String::new();

    let push_block = |block: DocumentBlock,
                      details: &mut Option<(usize, String, Vec<DocumentBlock>)>,
                      root: &mut Vec<DocumentBlock>| {
        if let Some((_, _, inner)) = details.as_mut() {
            inner.push(block);
        } else {
            root.push(block);
        }
    };

    for event in parser {
        if let Some((dest_url, mut alt_accum)) = active_image.take() {
            match event {
                Event::Text(t) | Event::Code(t) => {
                    alt_accum.push_str(&t);
                    active_image = Some((dest_url, alt_accum));
                    continue;
                }
                Event::End(TagEnd::Image) => {
                    if !inline_buffer.is_empty() && blockquote_depth == 0 && list_stack.is_empty() {
                        let spans = std::mem::take(&mut inline_buffer);
                        if spans.iter().any(|s| !s.text.trim().is_empty()) {
                            push_block(
                                DocumentBlock::Paragraph { spans },
                                &mut active_details,
                                &mut top_blocks,
                            );
                        }
                    }
                    if let Some(resolved_image) =
                        resolve_image_destination(&dest_url, document_dir)
                    {
                        let title = if alt_accum.trim().is_empty() {
                            resolved_image
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("Diagram")
                                .to_string()
                        } else {
                            alt_accum.trim().to_string()
                        };
                        push_block(
                            DocumentBlock::DiagramVisual {
                                alt_title: title,
                                asset: DiagramAssetSource::File(resolved_image),
                            },
                            &mut active_details,
                            &mut top_blocks,
                        );
                    }
                    continue;
                }
                _ => {
                    active_image = Some((dest_url, alt_accum));
                    continue;
                }
            }
        }

        if let Some((lang, mut code_buf)) = active_code_fence.take() {
            match event {
                Event::Text(t) => {
                    code_buf.push_str(&t);
                    active_code_fence = Some((lang, code_buf));
                }
                Event::End(TagEnd::CodeBlock) => {
                    let trimmed_lang = lang.trim().to_ascii_lowercase();
                    let is_mermaid = matches!(trimmed_lang.as_str(), "mermaid" | "mmd");
                    if is_mermaid && active_details.is_none() {
                        push_block(
                            DocumentBlock::DiagramVisual {
                                alt_title: "Mermaid Diagram".to_string(),
                                asset: DiagramAssetSource::InlineMermaid(
                                    code_buf.trim_end().to_string(),
                                ),
                            },
                            &mut active_details,
                            &mut top_blocks,
                        );
                    } else {
                        let lines = code_buf
                            .trim_end_matches('\n')
                            .lines()
                            .map(str::to_string)
                            .collect();
                        push_block(
                            DocumentBlock::CodeFence {
                                language: trimmed_lang,
                                lines,
                            },
                            &mut active_details,
                            &mut top_blocks,
                        );
                    }
                }
                _ => {
                    active_code_fence = Some((lang, code_buf));
                }
            }
            continue;
        }

        if in_table {
            match event {
                Event::Start(Tag::TableHead) => {
                    in_table_head = true;
                    current_row.clear();
                }
                Event::End(TagEnd::TableHead) => {
                    in_table_head = false;
                    table_headers = std::mem::take(&mut current_row);
                }
                Event::Start(Tag::TableRow) => {
                    current_row.clear();
                }
                Event::End(TagEnd::TableRow) => {
                    if !in_table_head {
                        table_rows.push(std::mem::take(&mut current_row));
                    }
                }
                Event::Start(Tag::TableCell) => {
                    current_cell_text.clear();
                }
                Event::End(TagEnd::TableCell) => {
                    current_row.push(current_cell_text.trim().to_string());
                    current_cell_text.clear();
                }
                Event::Text(t) | Event::Code(t) => {
                    current_cell_text.push_str(&t);
                }
                Event::End(TagEnd::Table) => {
                    in_table = false;
                    push_block(
                        DocumentBlock::Table {
                            headers: std::mem::take(&mut table_headers),
                            rows: std::mem::take(&mut table_rows),
                        },
                        &mut active_details,
                        &mut top_blocks,
                    );
                }
                _ => {}
            }
            continue;
        }

        match event {
            Event::Html(raw_html) | Event::InlineHtml(raw_html) => {
                let html_str = raw_html.as_ref();
                let lower = html_str.to_ascii_lowercase();

                if lower.contains("<details") && active_details.is_none() {
                    let summary =
                        extract_summary_from_html(html_str).unwrap_or_else(|| "Details".to_string());
                    let id = next_details_id;
                    next_details_id += 1;
                    active_details = Some((id, summary, Vec::new()));
                } else if lower.contains("<summary") && active_details.is_some() {
                    if let Some(extracted) = extract_summary_from_html(html_str)
                        && let Some((_, sum, _)) = active_details.as_mut()
                    {
                        *sum = extracted;
                    } else {
                        pending_summary_html = true;
                    }
                } else if lower.contains("</summary>") {
                    pending_summary_html = false;
                }

                if lower.contains("</details>")
                    && let Some((details_id, summary, inner_blocks)) = active_details.take()
                {
                    top_blocks.push(DocumentBlock::CollapsibleDetails {
                        details_id,
                        summary,
                        inner_blocks,
                    });
                }
            }
            Event::Start(Tag::Heading { level, .. }) => {
                current_heading_level = Some(heading_level_number(level));
                inline_buffer.clear();
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(level) = current_heading_level.take() {
                    let spans = std::mem::take(&mut inline_buffer);
                    push_block(
                        DocumentBlock::Heading { level, spans },
                        &mut active_details,
                        &mut top_blocks,
                    );
                }
            }
            Event::Start(Tag::Paragraph) => {
                in_paragraph = true;
                if pending_item_marker.is_none() {
                    inline_buffer.clear();
                }
            }
            Event::End(TagEnd::Paragraph) => {
                in_paragraph = false;
                let spans = std::mem::take(&mut inline_buffer);
                if spans.iter().any(|s| !s.text.trim().is_empty()) {
                    if blockquote_depth > 0 {
                        blockquote_paragraphs.push(spans);
                    } else if let Some(marker) = pending_item_marker.take() {
                        let depth = list_stack.len().saturating_sub(1);
                        push_block(
                            DocumentBlock::ListItem {
                                depth,
                                marker,
                                spans,
                            },
                            &mut active_details,
                            &mut top_blocks,
                        );
                    } else {
                        push_block(
                            DocumentBlock::Paragraph { spans },
                            &mut active_details,
                            &mut top_blocks,
                        );
                    }
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                blockquote_depth += 1;
                if blockquote_depth == 1 {
                    blockquote_paragraphs.clear();
                }
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                blockquote_depth = blockquote_depth.saturating_sub(1);
                if blockquote_depth == 0 && !blockquote_paragraphs.is_empty() {
                    let paragraphs = std::mem::take(&mut blockquote_paragraphs);
                    if let Some((alert_kind, cleaned_paragraphs)) =
                        detect_github_alert_paragraphs(&paragraphs)
                    {
                        push_block(
                            DocumentBlock::Alert {
                                kind: alert_kind,
                                paragraphs: cleaned_paragraphs,
                            },
                            &mut active_details,
                            &mut top_blocks,
                        );
                    } else {
                        push_block(
                            DocumentBlock::BlockQuote { paragraphs },
                            &mut active_details,
                            &mut top_blocks,
                        );
                    }
                }
            }
            Event::Start(Tag::List(first_number)) => {
                list_stack.push(first_number);
            }
            Event::End(TagEnd::List(_)) => {
                list_stack.pop();
            }
            Event::Start(Tag::Item) => {
                let marker = match list_stack.last_mut() {
                    Some(Some(num)) => {
                        let current = *num;
                        *num += 1;
                        format!("{current}.")
                    }
                    _ => "•".to_string(),
                };
                pending_item_marker = Some(marker);
                inline_buffer.clear();
            }
            Event::End(TagEnd::Item) => {
                if let Some(marker) = pending_item_marker.take() {
                    let spans = std::mem::take(&mut inline_buffer);
                    if spans.iter().any(|s| !s.text.trim().is_empty()) {
                        let depth = list_stack.len().saturating_sub(1);
                        push_block(
                            DocumentBlock::ListItem {
                                depth,
                                marker,
                                spans,
                            },
                            &mut active_details,
                            &mut top_blocks,
                        );
                    }
                }
            }
            Event::TaskListMarker(checked) => {
                pending_item_marker = Some(if checked {
                    "☑".to_string()
                } else {
                    "☐".to_string()
                });
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or("").to_string()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                active_code_fence = Some((lang, String::new()));
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                active_image = Some((dest_url.into_string(), String::new()));
            }
            Event::Start(Tag::Table(_)) => {
                in_table = true;
                table_headers.clear();
                table_rows.clear();
            }
            Event::Start(Tag::Strong) => {
                let mut next = style_stack.last().cloned().unwrap_or_default();
                next.bold = true;
                style_stack.push(next);
            }
            Event::End(TagEnd::Strong) => {
                if style_stack.len() > 1 {
                    style_stack.pop();
                }
            }
            Event::Start(Tag::Emphasis) => {
                let mut next = style_stack.last().cloned().unwrap_or_default();
                next.italic = true;
                style_stack.push(next);
            }
            Event::End(TagEnd::Emphasis) => {
                if style_stack.len() > 1 {
                    style_stack.pop();
                }
            }
            Event::Start(Tag::Strikethrough) => {
                let mut next = style_stack.last().cloned().unwrap_or_default();
                next.strikethrough = true;
                style_stack.push(next);
            }
            Event::End(TagEnd::Strikethrough) => {
                if style_stack.len() > 1 {
                    style_stack.pop();
                }
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let mut next = style_stack.last().cloned().unwrap_or_default();
                next.link_url = Some(dest_url.into_string());
                style_stack.push(next);
            }
            Event::End(TagEnd::Link) => {
                if style_stack.len() > 1 {
                    style_stack.pop();
                }
            }
            Event::Code(code_str) => {
                let mut code_style = style_stack.last().cloned().unwrap_or_default();
                code_style.inline_code = true;
                inline_buffer.push(StyledInlineSpan {
                    text: code_str.into_string(),
                    style: code_style,
                });
            }
            Event::Text(text_str) => {
                if pending_summary_html {
                    if let Some((_, sum, _)) = active_details.as_mut() {
                        *sum = text_str.trim().to_string();
                    }
                    pending_summary_html = false;
                } else if in_paragraph
                    || current_heading_level.is_some()
                    || pending_item_marker.is_some()
                    || blockquote_depth > 0
                {
                    let style = style_stack.last().cloned().unwrap_or_default();
                    inline_buffer.push(StyledInlineSpan {
                        text: text_str.into_string(),
                        style,
                    });
                }
            }
            Event::SoftBreak => {
                let style = style_stack.last().cloned().unwrap_or_default();
                inline_buffer.push(StyledInlineSpan {
                    text: " ".to_string(),
                    style,
                });
            }
            Event::HardBreak => {
                let style = style_stack.last().cloned().unwrap_or_default();
                inline_buffer.push(StyledInlineSpan {
                    text: "\n".to_string(),
                    style,
                });
            }
            Event::Rule => {
                push_block(
                    DocumentBlock::ThematicBreak,
                    &mut active_details,
                    &mut top_blocks,
                );
            }
            _ => {}
        }
    }

    if let Some((details_id, summary, inner_blocks)) = active_details.take() {
        top_blocks.push(DocumentBlock::CollapsibleDetails {
            details_id,
            summary,
            inner_blocks,
        });
    }

    let inferred_title = top_blocks
        .iter()
        .find_map(|block| match block {
            DocumentBlock::Heading { level: 1, spans } => {
                let combined: String = spans.iter().map(|s| s.text.as_str()).collect();
                let trimmed = combined.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            }
            _ => None,
        })
        .unwrap_or_else(|| fallback_title.to_string());

    ParsedMarkdownDocument {
        title: inferred_title,
        blocks: top_blocks,
    }
}

fn heading_level_number(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn extract_summary_from_html(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open_idx = lower.find("<summary")?;
    let after_open = &html[open_idx..];
    let gt_rel = after_open.find('>')?;
    let content_start = open_idx + gt_rel + 1;
    let rest_lower = &lower[content_start..];
    let close_rel = rest_lower.find("</summary>")?;
    let summary = html[content_start..content_start + close_rel].trim();
    (!summary.is_empty()).then(|| summary.to_string())
}

fn resolve_image_destination(dest_url: &str, document_dir: &Path) -> Option<PathBuf> {
    let trimmed = dest_url.trim();
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("data:")
    {
        return None;
    }
    let without_query = trimmed.split('?').next().unwrap_or(trimmed);
    if let Some(after_scheme) = without_query.strip_prefix("file://") {
        let slash_idx = after_scheme.find('/')?;
        let decoded = percent_decode_path(&after_scheme[slash_idx..]);
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
    let decoded = percent_decode_path(without_query);
    let bytes = decoded.as_bytes();
    let is_win_abs = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\');
    let path = Path::new(&decoded);
    if path.is_absolute() || is_win_abs {
        Some(path.to_path_buf())
    } else {
        Some(document_dir.join(path))
    }
}

fn detect_github_alert_paragraphs(
    paragraphs: &[Vec<StyledInlineSpan>],
) -> Option<(AlertKind, Vec<Vec<StyledInlineSpan>>)> {
    let first = paragraphs.first()?;
    let first_span = first.first()?;
    let trimmed = first_span.text.trim_start();

    let prefixes = [
        ("[!NOTE]", AlertKind::Note),
        ("[!TIP]", AlertKind::Tip),
        ("[!IMPORTANT]", AlertKind::Important),
        ("[!WARNING]", AlertKind::Warning),
        ("[!CAUTION]", AlertKind::Caution),
    ];

    for (marker, kind) in prefixes {
        if let Some(after) = trimmed.strip_prefix(marker) {
            let mut updated = paragraphs.to_vec();
            let remainder = after.trim_start_matches([' ', '\n']).to_string();
            if remainder.is_empty() {
                updated[0].remove(0);
                if updated[0].is_empty() {
                    updated.remove(0);
                }
            } else {
                updated[0][0].text = remainder;
            }
            return Some((kind, updated));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compiled_pele_markdown_with_svg_and_collapsed_details() {
        let sample = "# Auth Architecture\n\n> [!IMPORTANT] Token validation flow\n\n![Auth Flow](./diagrams/auth.svg)\n\n<details>\n<summary>Mermaid source</summary>\n\n```mermaid\nsequenceDiagram\n  Client->>API: Bearer JWT\n```\n\n</details>\n";
        let doc = parse_markdown_source(sample, Path::new("/tmp/diagrams"), "fallback.md");
        assert_eq!(doc.title, "Auth Architecture");
        assert!(matches!(
            doc.blocks[0],
            DocumentBlock::Heading { level: 1, .. }
        ));
        assert!(matches!(
            doc.blocks[1],
            DocumentBlock::Alert {
                kind: AlertKind::Important,
                ..
            }
        ));
        assert!(matches!(
            &doc.blocks[2],
            DocumentBlock::DiagramVisual {
                alt_title,
                asset: DiagramAssetSource::File(p),
            } if alt_title == "Auth Flow" && p == &Path::new("/tmp/diagrams").join("./diagrams/auth.svg")
        ));
        match &doc.blocks[3] {
            DocumentBlock::CollapsibleDetails {
                summary,
                inner_blocks,
                ..
            } => {
                assert_eq!(summary, "Mermaid source");
                assert_eq!(inner_blocks.len(), 1);
                assert!(matches!(
                    &inner_blocks[0],
                    DocumentBlock::CodeFence { language, .. } if language == "mermaid"
                ));
            }
            other => panic!("expected CollapsibleDetails, got {other:?}"),
        }
    }

    #[test]
    fn resolves_windows_file_url_diagram_images() {
        let sample = "![Win Diagram](file:///C:/Users/alice/My%20Diagrams/arch.svg)\n";
        let doc = parse_markdown_source(sample, Path::new("."), "win.md");
        assert!(matches!(
            &doc.blocks[0],
            DocumentBlock::DiagramVisual {
                alt_title,
                asset: DiagramAssetSource::File(p),
            } if alt_title == "Win Diagram" && p == Path::new("C:/Users/alice/My Diagrams/arch.svg")
        ));
    }
}
