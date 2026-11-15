use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{Frame, Terminal};
use unicode_width::UnicodeWidthStr;

use crate::diagram_renderer::{
    DiagramRasterEngine, TerminalCellMetrics, VisibleKittyPlacement,
};
use crate::discovery::{
    DiscoveredMarkdownFile, DiscoveryScope, extract_markdown_files_from_chat, home_directory,
    normalize_canonical_path, scan_diagrams_directory, scan_workspace_markdown_files,
};
use crate::document::{
    AlertKind, DocumentBlock, InlineStyle, ParsedMarkdownDocument, StyledInlineSpan,
    load_and_parse_markdown,
};
use crate::rex_bridge::{
    RexSessionContext, capture_terminal_scrollback, close_self_in_rex, detect_session_context,
    resolve_live_chat_block,
};
use crate::theme::{BackgroundThemeWatcher, RexThemePalette, initial_rex_palette};

#[derive(Debug, Clone)]
pub struct PeekLaunchOptions {
    pub session_id: Option<String>,
    pub workspace_cwd: PathBuf,
    pub mode: String,
    pub source_block: Option<String>,
    pub explicit_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
enum HitTarget {
    SelectScope(DiscoveryScope),
    SelectFileIndex(usize),
    ToggleSidebar,
    ToggleAllDetails,
    ToggleFollowChat,
    ZoomReset,
    OpenInEditor,
    OpenDefaultViewer,
    RefreshAll,
    CloseViewer,
    ToggleDetailsBlock(usize),
}

#[derive(Debug, Clone)]
struct MouseHitBox {
    rect: Rect,
    target: HitTarget,
}

#[derive(Debug, Clone)]
enum RenderedRow {
    TextLine {
        line: Line<'static>,
        details_toggle_id: Option<usize>,
    },
    DiagramSlot {
        image_id: u32,
        placement_id: u32,
        asset_cache_key: String,
        cell_cols: u16,
        total_cell_rows: u16,
        row_within_diagram: u16,
    },
}

struct PeekAppState {
    options: PeekLaunchOptions,
    rex_ctx: RexSessionContext,
    home_dir: PathBuf,
    workspace_cwd: PathBuf,
    source_chat_block: Option<String>,

    palette: RexThemePalette,
    theme_watcher: BackgroundThemeWatcher,
    cell_metrics: TerminalCellMetrics,

    scope: DiscoveryScope,
    chat_files: Vec<DiscoveredMarkdownFile>,
    diagram_files: Vec<DiscoveredMarkdownFile>,
    workspace_files: Vec<DiscoveredMarkdownFile>,
    workspace_scanned: bool,

    selected_path: Option<PathBuf>,
    selected_mtime_nanos: u128,
    parsed_doc: Option<ParsedMarkdownDocument>,
    parse_error: Option<String>,

    sidebar_open: bool,
    sidebar_scroll: usize,
    follow_chat: bool,
    zoom_pct: u16,
    expanded_details: HashSet<usize>,

    scroll_y: usize,
    viewport_width: u16,
    viewport_height: u16,
    rendered_rows: Vec<RenderedRow>,
    layout_dirty: bool,

    raster_engine: DiagramRasterEngine,
    pending_placements: Vec<VisibleKittyPlacement>,
    hitboxes: Vec<MouseHitBox>,
    toast: Option<(String, Instant)>,

    last_poll: Instant,
    should_quit: bool,
}

impl PeekAppState {
    fn new(options: PeekLaunchOptions) -> Self {
        let rex_ctx = detect_session_context(options.session_id.clone());
        let home_dir = home_directory();
        let workspace_cwd = options.workspace_cwd.clone();
        let source_chat_block = options.source_block.clone();

        let palette = initial_rex_palette();
        let theme_watcher = BackgroundThemeWatcher::spawn(palette.theme_id.clone());
        let cell_metrics = TerminalCellMetrics::detect();
        let follow_chat = options.mode == "dock" && options.explicit_path.is_none();

        let mut app = Self {
            options,
            rex_ctx,
            home_dir,
            workspace_cwd,
            source_chat_block,
            palette,
            theme_watcher,
            cell_metrics,
            scope: DiscoveryScope::Chat,
            chat_files: Vec::new(),
            diagram_files: Vec::new(),
            workspace_files: Vec::new(),
            workspace_scanned: false,
            selected_path: None,
            selected_mtime_nanos: 0,
            parsed_doc: None,
            parse_error: None,
            sidebar_open: true,
            sidebar_scroll: 0,
            follow_chat,
            zoom_pct: 100,
            expanded_details: HashSet::new(),
            scroll_y: 0,
            viewport_width: 0,
            viewport_height: 0,
            rendered_rows: Vec::new(),
            layout_dirty: true,
            raster_engine: DiagramRasterEngine::new(),
            pending_placements: Vec::new(),
            hitboxes: Vec::new(),
            toast: None,
            last_poll: Instant::now(),
            should_quit: false,
        };

        app.refresh_discovered_files(true, false);
        app
    }

    fn active_scope_files(&self) -> &[DiscoveredMarkdownFile] {
        match self.scope {
            DiscoveryScope::Chat => &self.chat_files,
            DiscoveryScope::Diagrams => &self.diagram_files,
            DiscoveryScope::Workspace => &self.workspace_files,
        }
    }

    fn ensure_workspace_scanned(&mut self) {
        if !self.workspace_scanned {
            self.workspace_files =
                scan_workspace_markdown_files(&self.workspace_cwd, &self.home_dir);
            self.workspace_scanned = true;
        }
    }

    fn refresh_discovered_files(&mut self, initial: bool, manual_sync: bool) {
        if let Some(sid) = self.rex_ctx.session_id.as_deref() {
            if (self.source_chat_block.is_none() || manual_sync)
                && let Some((live_blk, live_cwd)) = resolve_live_chat_block(
                    sid,
                    self.rex_ctx.self_block_id.as_deref(),
                    self.source_chat_block.as_deref(),
                )
            {
                self.source_chat_block = Some(live_blk);
                if let Some(cwd) = live_cwd {
                    self.workspace_cwd = cwd;
                    self.workspace_scanned = false;
                }
            }

            if let Some(chat_blk) = self.source_chat_block.as_deref()
                && let Some(capture) = capture_terminal_scrollback(sid, chat_blk)
            {
                self.chat_files = extract_markdown_files_from_chat(
                    &capture,
                    &self.workspace_cwd,
                    &self.home_dir,
                );
            }
        }

        self.diagram_files = scan_diagrams_directory(&self.workspace_cwd, &self.home_dir);

        if manual_sync {
            if self.scope == DiscoveryScope::Workspace {
                self.workspace_scanned = false;
                self.ensure_workspace_scanned();
            } else {
                self.workspace_scanned = false;
            }
        }

        if initial {
            if let Some(explicit) = self.options.explicit_path.clone() {
                let resolved = if explicit.is_absolute() {
                    explicit
                } else {
                    self.workspace_cwd.join(explicit)
                };
                if resolved.is_file() {
                    let canonical = normalize_canonical_path(&resolved);
                    if !self.chat_files.iter().any(|f| f.path == canonical) {
                        self.chat_files.insert(
                            0,
                            DiscoveredMarkdownFile {
                                display_name: canonical
                                    .file_name()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("document.md")
                                    .to_string(),
                                parent_hint: canonical
                                    .parent()
                                    .map(|p| p.to_string_lossy().into_owned())
                                    .unwrap_or_default(),
                                path: canonical.clone(),
                                scope: DiscoveryScope::Chat,
                                modified_epoch_secs: 0,
                            },
                        );
                    }
                    self.scope = DiscoveryScope::Chat;
                    self.select_document(canonical, true);
                    return;
                }
            }

            if let Some(first_chat) = self.chat_files.first() {
                self.scope = DiscoveryScope::Chat;
                self.select_document(first_chat.path.clone(), true);
            } else if let Some(first_diag) = self.diagram_files.first() {
                self.scope = DiscoveryScope::Diagrams;
                self.select_document(first_diag.path.clone(), true);
            } else {
                self.ensure_workspace_scanned();
                if let Some(first_ws) = self.workspace_files.first() {
                    self.scope = DiscoveryScope::Workspace;
                    self.select_document(first_ws.path.clone(), true);
                }
            }
            return;
        }

        if self.follow_chat
            && let Some(newest_chat) = self.chat_files.first()
            && self.selected_path.as_ref() != Some(&newest_chat.path)
        {
            self.scope = DiscoveryScope::Chat;
            self.select_document(newest_chat.path.clone(), true);
            return;
        }

        if self.selected_path.is_none()
            && let Some(first) = self.active_scope_files().first()
        {
            self.select_document(first.path.clone(), true);
        }
    }

    fn select_document(&mut self, path: PathBuf, reset_scroll: bool) {
        let mtime = read_mtime_nanos(&path);
        match load_and_parse_markdown(&path) {
            Ok(doc) => {
                self.parsed_doc = Some(doc);
                self.parse_error = None;
            }
            Err(err) => {
                self.parsed_doc = None;
                self.parse_error = Some(err.to_string());
            }
        }
        self.selected_path = Some(path);
        self.selected_mtime_nanos = mtime;
        if reset_scroll {
            self.scroll_y = 0;
            self.expanded_details.clear();
        }
        self.layout_dirty = true;
        self.raster_engine.invalidate_sent_placements();
    }

    fn poll_live_updates(&mut self) {
        if let Some(updated_palette) = self.theme_watcher.try_recv_updated_palette() {
            self.palette = updated_palette;
            self.raster_engine.clear_cached_rasters();
            self.layout_dirty = true;
        }

        if self.last_poll.elapsed() >= Duration::from_millis(850) {
            self.last_poll = Instant::now();
            if let Some(current_path) = self.selected_path.clone() {
                let latest_mtime = read_mtime_nanos(&current_path);
                if latest_mtime != 0 && latest_mtime != self.selected_mtime_nanos {
                    self.select_document(current_path, false);
                }
            }
            if self.follow_chat {
                self.refresh_discovered_files(false, false);
            }
        }

        if let Some((_, created)) = &self.toast
            && created.elapsed() >= Duration::from_secs(3)
        {
            self.toast = None;
        }
    }

    fn switch_scope(&mut self, next_scope: DiscoveryScope) {
        if next_scope == DiscoveryScope::Workspace {
            self.ensure_workspace_scanned();
        }
        self.scope = next_scope;
        self.sidebar_scroll = 0;
        let files = self.active_scope_files();
        if let Some(sel) = self.selected_path.as_ref()
            && files.iter().any(|f| &f.path == sel)
        {
            return;
        }
        if let Some(first) = files.first().cloned() {
            self.select_document(first.path, true);
        }
    }

    fn step_file_selection(&mut self, delta: isize) {
        if self.scope == DiscoveryScope::Workspace {
            self.ensure_workspace_scanned();
        }
        let files = self.active_scope_files();
        if files.is_empty() {
            return;
        }
        let current_idx = self
            .selected_path
            .as_ref()
            .and_then(|sel| files.iter().position(|f| &f.path == sel))
            .unwrap_or(0);
        let next_idx = if delta >= 0 {
            (current_idx + delta as usize).min(files.len() - 1)
        } else {
            current_idx.saturating_sub(delta.unsigned_abs())
        };
        let target = files[next_idx].path.clone();
        self.follow_chat = false;
        self.select_document(target, true);
    }

    fn adjust_zoom(&mut self, delta: i16) {
        let updated = (self.zoom_pct as i16 + delta).clamp(50, 220) as u16;
        if updated != self.zoom_pct {
            self.zoom_pct = updated;
            self.layout_dirty = true;
            self.raster_engine.invalidate_sent_placements();
            self.set_toast(format!("Diagram zoom: {}%", self.zoom_pct));
        }
    }

    fn toggle_all_details(&mut self) {
        let Some(doc) = &self.parsed_doc else { return };
        let all_ids: Vec<usize> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                DocumentBlock::CollapsibleDetails { details_id, .. } => Some(*details_id),
                _ => None,
            })
            .collect();
        if all_ids.is_empty() {
            return;
        }
        if self.expanded_details.len() == all_ids.len() {
            self.expanded_details.clear();
        } else {
            self.expanded_details = all_ids.into_iter().collect();
        }
        self.layout_dirty = true;
        self.raster_engine.invalidate_sent_placements();
    }

    fn open_current_in_editor(&mut self) {
        let Some(path) = self.selected_path.as_ref() else {
            return;
        };
        if let Some(editor_name) = spawn_preferred_editor(path) {
            self.set_toast(format!("Opened {} in {editor_name}", path.display()));
        } else {
            let _ = spawn_system_default_opener(path);
            self.set_toast(format!("Opened {}", path.display()));
        }
    }

    fn open_current_in_default_viewer(&mut self) {
        let Some(path) = self.selected_path.as_ref() else {
            return;
        };
        let _ = spawn_system_default_opener(path);
        self.set_toast(format!("Opened {}", path.display()));
    }

    fn set_toast(&mut self, msg: String) {
        self.toast = Some((msg, Instant::now()));
    }

    fn rebuild_document_rows(&mut self, inner_width: u16) {
        self.rendered_rows.clear();
        let text_width = inner_width.saturating_sub(4).max(16) as usize;

        if let Some(err) = &self.parse_error {
            self.rendered_rows.push(RenderedRow::TextLine {
                line: Line::from(Span::styled(
                    format!("  Error loading Markdown: {err}"),
                    Style::default().fg(self.palette.caution_accent),
                )),
                details_toggle_id: None,
            });
            self.layout_dirty = false;
            return;
        }

        let Some(doc) = self.parsed_doc.clone() else {
            self.rendered_rows.push(RenderedRow::TextLine {
                line: Line::from(Span::styled(
                    "  No Markdown files found in active scope. Press 2 for Diagrams or 3 for Workspace.",
                    Style::default().fg(self.palette.muted),
                )),
                details_toggle_id: None,
            });
            self.layout_dirty = false;
            return;
        };

        let mut next_placement_ordinal: u32 = 1;
        self.rendered_rows.push(RenderedRow::TextLine {
            line: Line::default(),
            details_toggle_id: None,
        });

        for block in &doc.blocks {
            self.append_rendered_block(
                block,
                text_width,
                inner_width,
                &mut next_placement_ordinal,
            );
        }

        let max_scroll = self
            .rendered_rows
            .len()
            .saturating_sub(self.viewport_height.max(1) as usize);
        if self.scroll_y > max_scroll {
            self.scroll_y = max_scroll;
        }
        self.layout_dirty = false;
    }

    fn append_rendered_block(
        &mut self,
        block: &DocumentBlock,
        text_width: usize,
        inner_width: u16,
        next_placement_ordinal: &mut u32,
    ) {
        let p = &self.palette;
        match block {
            DocumentBlock::Heading { level, spans } => {
                let heading_color = match level {
                    1 => p.heading_primary,
                    2 => p.heading_secondary,
                    _ => p.heading_tertiary,
                };
                let prefix = match level {
                    1 => "  ",
                    2 => "  ▍ ",
                    _ => "  • ",
                };
                let wrapped = wrap_styled_spans(
                    spans,
                    text_width.saturating_sub(4),
                    p,
                    Some(heading_color),
                );
                for (idx, mut row_spans) in wrapped.into_iter().enumerate() {
                    let lead = if idx == 0 { prefix } else { "    " };
                    row_spans.insert(
                        0,
                        Span::styled(
                            lead.to_string(),
                            Style::default()
                                .fg(heading_color)
                                .add_modifier(Modifier::BOLD),
                        ),
                    );
                    self.rendered_rows.push(RenderedRow::TextLine {
                        line: Line::from(row_spans),
                        details_toggle_id: None,
                    });
                }
                if *level == 1 {
                    let rule = "─".repeat(text_width.min(72));
                    self.rendered_rows.push(RenderedRow::TextLine {
                        line: Line::from(Span::styled(
                            format!("  {rule}"),
                            Style::default().fg(p.border),
                        )),
                        details_toggle_id: None,
                    });
                }
                self.push_blank_line();
            }
            DocumentBlock::Paragraph { spans } => {
                for mut row_spans in wrap_styled_spans(spans, text_width, p, None) {
                    row_spans.insert(0, Span::raw("  "));
                    self.rendered_rows.push(RenderedRow::TextLine {
                        line: Line::from(row_spans),
                        details_toggle_id: None,
                    });
                }
                self.push_blank_line();
            }
            DocumentBlock::Alert { kind, paragraphs } => {
                let accent = match kind {
                    AlertKind::Note => p.note_accent,
                    AlertKind::Tip => p.tip_accent,
                    AlertKind::Important => p.important_accent,
                    AlertKind::Warning => p.warning_accent,
                    AlertKind::Caution => p.caution_accent,
                };
                self.rendered_rows.push(RenderedRow::TextLine {
                    line: Line::from(vec![
                        Span::styled("  ┃ ", Style::default().fg(accent)),
                        Span::styled(
                            kind.badge_label().to_string(),
                            Style::default().fg(accent).add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    details_toggle_id: None,
                });
                for para in paragraphs {
                    for mut row_spans in
                        wrap_styled_spans(para, text_width.saturating_sub(4), p, None)
                    {
                        row_spans.insert(0, Span::styled("  ┃ ", Style::default().fg(accent)));
                        self.rendered_rows.push(RenderedRow::TextLine {
                            line: Line::from(row_spans),
                            details_toggle_id: None,
                        });
                    }
                }
                self.push_blank_line();
            }
            DocumentBlock::BlockQuote { paragraphs } => {
                for para in paragraphs {
                    for mut row_spans in
                        wrap_styled_spans(para, text_width.saturating_sub(4), p, Some(p.muted))
                    {
                        row_spans.insert(0, Span::styled("  │ ", Style::default().fg(p.border)));
                        self.rendered_rows.push(RenderedRow::TextLine {
                            line: Line::from(row_spans),
                            details_toggle_id: None,
                        });
                    }
                }
                self.push_blank_line();
            }
            DocumentBlock::ListItem {
                depth,
                marker,
                spans,
            } => {
                let indent = "  ".repeat(depth + 1);
                let marker_prefix = format!("{indent}{marker} ");
                let cont_prefix = " ".repeat(UnicodeWidthStr::width(marker_prefix.as_str()));
                let avail = text_width.saturating_sub(cont_prefix.len()).max(12);
                for (idx, mut row_spans) in wrap_styled_spans(spans, avail, p, None)
                    .into_iter()
                    .enumerate()
                {
                    if idx == 0 {
                        row_spans.insert(
                            0,
                            Span::styled(
                                marker_prefix.clone(),
                                Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                            ),
                        );
                    } else {
                        row_spans.insert(0, Span::raw(cont_prefix.clone()));
                    }
                    self.rendered_rows.push(RenderedRow::TextLine {
                        line: Line::from(row_spans),
                        details_toggle_id: None,
                    });
                }
            }
            DocumentBlock::CodeFence { language, lines } => {
                let header_label = if language.is_empty() {
                    "code".to_string()
                } else {
                    language.clone()
                };
                self.rendered_rows.push(RenderedRow::TextLine {
                    line: Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            format!("╭─ {header_label} "),
                            Style::default().fg(p.dim),
                        ),
                    ]),
                    details_toggle_id: None,
                });
                for code_line in lines {
                    let clipped: String =
                        code_line.chars().take(text_width.saturating_sub(2)).collect();
                    self.rendered_rows.push(RenderedRow::TextLine {
                        line: Line::from(vec![
                            Span::styled("  │ ", Style::default().fg(p.dim)),
                            Span::styled(
                                format!("{clipped:<width$}", width = text_width.saturating_sub(4)),
                                Style::default().fg(p.fg).bg(p.code_bg),
                            ),
                        ]),
                        details_toggle_id: None,
                    });
                }
                self.rendered_rows.push(RenderedRow::TextLine {
                    line: Line::from(Span::styled("  ╰────", Style::default().fg(p.dim))),
                    details_toggle_id: None,
                });
                self.push_blank_line();
            }
            DocumentBlock::Table { headers, rows } => {
                for table_line in format_table_lines(headers, rows, text_width, p) {
                    self.rendered_rows.push(RenderedRow::TextLine {
                        line: table_line,
                        details_toggle_id: None,
                    });
                }
                self.push_blank_line();
            }
            DocumentBlock::DiagramVisual { alt_title, asset } => {
                let avail_diagram_cols = inner_width.saturating_sub(4).max(16);
                let palette_snapshot = self.palette.clone();
                if let Some((asset_key, image_id, cell_cols, cell_rows)) =
                    self.raster_engine.prepare_diagram_layout(
                        asset,
                        avail_diagram_cols,
                        self.zoom_pct,
                        self.cell_metrics,
                        &palette_snapshot,
                    )
                {
                    let placement_id = *next_placement_ordinal;
                    *next_placement_ordinal += 1;

                    self.rendered_rows.push(RenderedRow::TextLine {
                        line: Line::from(vec![
                            Span::styled("  ◈ ", Style::default().fg(self.palette.accent)),
                            Span::styled(
                                alt_title.clone(),
                                Style::default()
                                    .fg(self.palette.muted)
                                    .add_modifier(Modifier::ITALIC),
                            ),
                        ]),
                        details_toggle_id: None,
                    });

                    for row_idx in 0..cell_rows {
                        self.rendered_rows.push(RenderedRow::DiagramSlot {
                            image_id,
                            placement_id,
                            asset_cache_key: asset_key.clone(),
                            cell_cols,
                            total_cell_rows: cell_rows,
                            row_within_diagram: row_idx,
                        });
                    }
                    self.push_blank_line();
                } else {
                    self.rendered_rows.push(RenderedRow::TextLine {
                        line: Line::from(Span::styled(
                            format!("  [Diagram missing or unrenderable: {alt_title}]"),
                            Style::default().fg(self.palette.warning_accent),
                        )),
                        details_toggle_id: None,
                    });
                    self.push_blank_line();
                }
            }
            DocumentBlock::CollapsibleDetails {
                details_id,
                summary,
                inner_blocks,
            } => {
                let is_open = self.expanded_details.contains(details_id);
                let arrow = if is_open { "▾" } else { "▸" };
                let hint = if is_open {
                    " (click or press d to collapse)"
                } else {
                    " (click or press d to view source)"
                };
                self.rendered_rows.push(RenderedRow::TextLine {
                    line: Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            format!("{arrow} {summary}"),
                            Style::default()
                                .fg(self.palette.accent)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(hint.to_string(), Style::default().fg(self.palette.dim)),
                    ]),
                    details_toggle_id: Some(*details_id),
                });
                if is_open {
                    for inner in inner_blocks {
                        self.append_rendered_block(
                            inner,
                            text_width,
                            inner_width,
                            next_placement_ordinal,
                        );
                    }
                } else {
                    self.push_blank_line();
                }
            }
            DocumentBlock::ThematicBreak => {
                let rule = "─".repeat(text_width);
                self.rendered_rows.push(RenderedRow::TextLine {
                    line: Line::from(Span::styled(
                        format!("  {rule}"),
                        Style::default().fg(p.border),
                    )),
                    details_toggle_id: None,
                });
                self.push_blank_line();
            }
        }
    }

    fn push_blank_line(&mut self) {
        self.rendered_rows.push(RenderedRow::TextLine {
            line: Line::default(),
            details_toggle_id: None,
        });
    }
}

pub fn run_peek_tui(options: PeekLaunchOptions) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = PeekAppState::new(options.clone());

    let loop_result = (|| -> Result<()> {
        while !app.should_quit {
            app.poll_live_updates();

            terminal.draw(|frame| draw_ui(frame, &mut app))?;
            app.raster_engine
                .emit_visible_placements(terminal.backend_mut(), &app.pending_placements)?;

            if event::poll(Duration::from_millis(40))? {
                dispatch_terminal_event(&mut app, event::read()?);
                while !app.should_quit && event::poll(Duration::ZERO)? {
                    dispatch_terminal_event(&mut app, event::read()?);
                }
            }
        }
        Ok(())
    })();

    let _ = app
        .raster_engine
        .purge_all_kitty_textures(terminal.backend_mut());
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    close_self_in_rex(
        app.rex_ctx.session_id.as_deref(),
        app.rex_ctx.self_block_id.as_deref(),
        &options.mode,
    );

    loop_result
}

fn dispatch_terminal_event(app: &mut PeekAppState, ev: Event) {
    match ev {
        Event::Key(key_ev) => handle_key_event(app, key_ev),
        Event::Mouse(mouse_ev) => handle_mouse_event(app, mouse_ev),
        Event::Resize(_, _) => {
            app.cell_metrics = TerminalCellMetrics::detect();
            app.layout_dirty = true;
            app.raster_engine.invalidate_sent_placements();
        }
        _ => {}
    }
}

fn draw_ui(frame: &mut Frame, app: &mut PeekAppState) {
    app.hitboxes.clear();
    app.pending_placements.clear();

    let full_area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(app.palette.bg).fg(app.palette.fg)),
        full_area,
    );

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(full_area);

    draw_top_bar(frame, app, vertical[0]);

    let show_sidebar = app.sidebar_open && full_area.width >= 76;
    let content_area = if show_sidebar {
        let sidebar_width = (full_area.width / 4).clamp(24, 34);
        let horiz = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(sidebar_width), Constraint::Min(24)])
            .split(vertical[1]);
        draw_sidebar(frame, app, horiz[0]);
        horiz[1]
    } else {
        vertical[1]
    };

    draw_document_viewport(frame, app, content_area);
    draw_status_footer(frame, app, vertical[2]);
}

fn draw_top_bar(frame: &mut Frame, app: &mut PeekAppState, area: Rect) {
    let p = app.palette.clone();
    frame.render_widget(
        Block::default().style(Style::default().bg(p.chrome)),
        area,
    );

    let mut x_cursor = area.x + 1;
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];

    let scopes = [
        (DiscoveryScope::Chat, "1", Some(app.chat_files.len())),
        (DiscoveryScope::Diagrams, "2", Some(app.diagram_files.len())),
        (
            DiscoveryScope::Workspace,
            "3",
            app.workspace_scanned.then_some(app.workspace_files.len()),
        ),
    ];

    for (sc, key_hint, maybe_count) in scopes {
        let label = match maybe_count {
            Some(count) => format!(" {key_hint}:{} ({count}) ", sc.label()),
            None => format!(" {key_hint}:{} ", sc.label()),
        };
        let w = UnicodeWidthStr::width(label.as_str()) as u16;
        let is_active = app.scope == sc;
        let style = if is_active {
            Style::default()
                .fg(p.bg)
                .bg(p.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.muted).bg(p.surface)
        };
        spans.push(Span::styled(label, style));
        app.hitboxes.push(MouseHitBox {
            rect: Rect::new(x_cursor, area.y, w, 1),
            target: HitTarget::SelectScope(sc),
        });
        x_cursor += w + 1;
        spans.push(Span::raw(" "));
    }

    let buttons: Vec<(String, HitTarget)> = vec![
        (" [b] Files ".to_string(), HitTarget::ToggleSidebar),
        (" [d] Source ".to_string(), HitTarget::ToggleAllDetails),
        (
            format!(
                " [f] Follow:{} ",
                if app.follow_chat { "ON" } else { "OFF" }
            ),
            HitTarget::ToggleFollowChat,
        ),
        (format!(" [-/+] {}% ", app.zoom_pct), HitTarget::ZoomReset),
        (" [e] Edit ".to_string(), HitTarget::OpenInEditor),
        (" [o] Open ".to_string(), HitTarget::OpenDefaultViewer),
        (" [r] Sync ".to_string(), HitTarget::RefreshAll),
        (" [q] Close ".to_string(), HitTarget::CloseViewer),
    ];

    for (btn_label, target) in buttons {
        let w = UnicodeWidthStr::width(btn_label.as_str()) as u16;
        if x_cursor + w >= area.x + area.width {
            break;
        }
        spans.push(Span::styled(
            btn_label,
            Style::default().fg(p.fg).bg(p.surface),
        ));
        app.hitboxes.push(MouseHitBox {
            rect: Rect::new(x_cursor, area.y, w, 1),
            target,
        });
        x_cursor += w + 1;
        spans.push(Span::raw(" "));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_sidebar(frame: &mut Frame, app: &mut PeekAppState, area: Rect) {
    let p = app.palette.clone();
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(p.border))
        .style(Style::default().bg(p.chrome));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let files = app.active_scope_files().to_vec();
    if files.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            " (empty)",
            Style::default().fg(p.dim),
        )));
        frame.render_widget(empty, inner);
        return;
    }

    let selected_idx = app
        .selected_path
        .as_ref()
        .and_then(|sel| files.iter().position(|f| &f.path == sel))
        .unwrap_or(0);

    let max_items = (inner.height / 2).max(1) as usize;
    if selected_idx < app.sidebar_scroll {
        app.sidebar_scroll = selected_idx;
    } else if selected_idx >= app.sidebar_scroll + max_items {
        app.sidebar_scroll = selected_idx + 1 - max_items;
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut row_y = inner.y;
    let avail_w = inner.width.saturating_sub(2) as usize;

    for (idx, item) in files.iter().enumerate().skip(app.sidebar_scroll) {
        if row_y + 1 >= inner.y + inner.height {
            break;
        }
        let is_sel = Some(&item.path) == app.selected_path.as_ref();
        let row_bg = if is_sel { p.selected_bg } else { p.chrome };
        let name_fg = if is_sel { p.accent } else { p.fg };
        let indicator = if is_sel { "▎" } else { " " };

        let name_trunc: String = item.display_name.chars().take(avail_w).collect();
        let hint_trunc: String = item.parent_hint.chars().take(avail_w).collect();

        lines.push(Line::from(vec![
            Span::styled(indicator, Style::default().fg(p.accent).bg(row_bg)),
            Span::styled(
                format!("{name_trunc:<width$}", width = avail_w),
                Style::default()
                    .fg(name_fg)
                    .bg(row_bg)
                    .add_modifier(if is_sel {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled(indicator, Style::default().fg(p.accent).bg(row_bg)),
            Span::styled(
                format!("{hint_trunc:<width$}", width = avail_w),
                Style::default().fg(p.dim).bg(row_bg),
            ),
        ]));

        app.hitboxes.push(MouseHitBox {
            rect: Rect::new(inner.x, row_y, inner.width, 2),
            target: HitTarget::SelectFileIndex(idx),
        });
        row_y += 2;
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_document_viewport(frame: &mut Frame, app: &mut PeekAppState, area: Rect) {
    if area.width != app.viewport_width || area.height != app.viewport_height {
        app.viewport_width = area.width;
        app.viewport_height = area.height;
        app.layout_dirty = true;
        app.raster_engine.invalidate_sent_placements();
    }

    if app.layout_dirty {
        app.rebuild_document_rows(area.width);
    }

    let max_scroll = app
        .rendered_rows
        .len()
        .saturating_sub(area.height.max(1) as usize);
    if app.scroll_y > max_scroll {
        app.scroll_y = max_scroll;
    }

    let mut visible_lines: Vec<Line<'static>> = Vec::with_capacity(area.height as usize);
    let mut active_diagram_run: Option<VisibleKittyPlacement> = None;

    for screen_offset in 0..(area.height as usize) {
        let doc_row_idx = app.scroll_y + screen_offset;
        let screen_y = area.y + screen_offset as u16;

        match app.rendered_rows.get(doc_row_idx) {
            Some(RenderedRow::TextLine {
                line,
                details_toggle_id,
            }) => {
                if let Some(finished) = active_diagram_run.take() {
                    app.pending_placements.push(finished);
                }
                if let Some(det_id) = details_toggle_id {
                    app.hitboxes.push(MouseHitBox {
                        rect: Rect::new(area.x, screen_y, area.width, 1),
                        target: HitTarget::ToggleDetailsBlock(*det_id),
                    });
                }
                visible_lines.push(line.clone());
            }
            Some(RenderedRow::DiagramSlot {
                image_id,
                placement_id,
                asset_cache_key,
                cell_cols,
                total_cell_rows,
                row_within_diagram,
            }) => {
                visible_lines.push(Line::default());
                match active_diagram_run.as_mut() {
                    Some(run)
                        if run.image_id == *image_id && run.placement_id == *placement_id =>
                    {
                        run.visible_cell_rows += 1;
                    }
                    _ => {
                        if let Some(prev) = active_diagram_run.take() {
                            app.pending_placements.push(prev);
                        }
                        active_diagram_run = Some(VisibleKittyPlacement {
                            image_id: *image_id,
                            placement_id: *placement_id,
                            asset_cache_key: asset_cache_key.clone(),
                            screen_col_1idx: area.x + 3,
                            screen_row_1idx: screen_y + 1,
                            cell_cols: *cell_cols,
                            total_cell_rows: *total_cell_rows,
                            visible_row_offset: *row_within_diagram,
                            visible_cell_rows: 1,
                        });
                    }
                }
            }
            None => {
                if let Some(finished) = active_diagram_run.take() {
                    app.pending_placements.push(finished);
                }
                visible_lines.push(Line::default());
            }
        }
    }

    if let Some(finished) = active_diagram_run.take() {
        app.pending_placements.push(finished);
    }

    frame.render_widget(
        Paragraph::new(visible_lines).style(Style::default().bg(app.palette.bg).fg(app.palette.fg)),
        area,
    );
}

fn draw_status_footer(frame: &mut Frame, app: &PeekAppState, area: Rect) {
    let p = &app.palette;
    frame.render_widget(
        Block::default().style(Style::default().bg(p.chrome)),
        area,
    );

    let left_text = if let Some((toast_msg, _)) = &app.toast {
        format!("  {toast_msg}")
    } else if let Some(sel) = &app.selected_path {
        if let Some(doc) = &app.parsed_doc {
            format!("  {} · {}", doc.title, sel.display())
        } else {
            format!("  {}", sel.display())
        }
    } else {
        "  rex-mdpeek · Pele + Kitty Graphics".to_string()
    };

    let total_rows = app.rendered_rows.len().max(1);
    let pct = (((app.scroll_y + app.viewport_height as usize).min(total_rows) * 100) / total_rows)
        .min(100);
    let right_hint = format!(" j/k scroll · [/] file · +/- zoom · d source · e edit · {pct}% ");

    let avail_left = (area.width as usize).saturating_sub(right_hint.len() + 1);
    let clipped_left: String = left_text.chars().take(avail_left).collect();
    let pad = avail_left.saturating_sub(UnicodeWidthStr::width(clipped_left.as_str()));

    let line = Line::from(vec![
        Span::styled(clipped_left, Style::default().fg(p.muted)),
        Span::raw(" ".repeat(pad)),
        Span::styled(right_hint, Style::default().fg(p.dim)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn handle_key_event(app: &mut PeekAppState, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('c') if ctrl => app.should_quit = true,
        KeyCode::Char('1') => app.switch_scope(DiscoveryScope::Chat),
        KeyCode::Char('2') => app.switch_scope(DiscoveryScope::Diagrams),
        KeyCode::Char('3') => app.switch_scope(DiscoveryScope::Workspace),
        KeyCode::Tab => {
            let next = match app.scope {
                DiscoveryScope::Chat => DiscoveryScope::Diagrams,
                DiscoveryScope::Diagrams => DiscoveryScope::Workspace,
                DiscoveryScope::Workspace => DiscoveryScope::Chat,
            };
            app.switch_scope(next);
        }
        KeyCode::Down | KeyCode::Char('j') if !ctrl => {
            app.scroll_y = app.scroll_y.saturating_add(2);
        }
        KeyCode::Up | KeyCode::Char('k') if !ctrl => {
            app.scroll_y = app.scroll_y.saturating_sub(2);
        }
        KeyCode::Char('d') if ctrl => {
            let half = (app.viewport_height / 2).max(4) as usize;
            app.scroll_y = app.scroll_y.saturating_add(half);
        }
        KeyCode::Char('u') if ctrl => {
            let half = (app.viewport_height / 2).max(4) as usize;
            app.scroll_y = app.scroll_y.saturating_sub(half);
        }
        KeyCode::PageDown | KeyCode::Char(' ') => {
            let step = app.viewport_height.saturating_sub(3).max(4) as usize;
            app.scroll_y = app.scroll_y.saturating_add(step);
        }
        KeyCode::PageUp => {
            let step = app.viewport_height.saturating_sub(3).max(4) as usize;
            app.scroll_y = app.scroll_y.saturating_sub(step);
        }
        KeyCode::Char('g') | KeyCode::Home => {
            app.scroll_y = 0;
        }
        KeyCode::Char('G') | KeyCode::End => {
            app.scroll_y = app.rendered_rows.len();
        }
        KeyCode::Char(']') | KeyCode::Char('n') => app.step_file_selection(1),
        KeyCode::Char('[') | KeyCode::Char('p') => app.step_file_selection(-1),
        KeyCode::Char('+') | KeyCode::Char('=') => app.adjust_zoom(20),
        KeyCode::Char('-') => app.adjust_zoom(-20),
        KeyCode::Char('0') => {
            app.zoom_pct = 100;
            app.layout_dirty = true;
            app.raster_engine.invalidate_sent_placements();
            app.set_toast("Diagram zoom: 100%".to_string());
        }
        KeyCode::Char('b') => {
            app.sidebar_open = !app.sidebar_open;
            app.layout_dirty = true;
            app.raster_engine.invalidate_sent_placements();
        }
        KeyCode::Char('d') | KeyCode::Enter => app.toggle_all_details(),
        KeyCode::Char('f') => {
            app.follow_chat = !app.follow_chat;
            app.set_toast(format!(
                "Follow latest chat .md: {}",
                if app.follow_chat { "ON" } else { "OFF" }
            ));
        }
        KeyCode::Char('r') => {
            app.raster_engine.clear_cached_rasters();
            app.refresh_discovered_files(false, true);
            if let Some(sel) = app.selected_path.clone() {
                app.select_document(sel, false);
            }
            app.set_toast("Synced chat & reloaded Markdown".to_string());
        }
        KeyCode::Char('e') => app.open_current_in_editor(),
        KeyCode::Char('o') => app.open_current_in_default_viewer(),
        _ => {}
    }
}

fn handle_mouse_event(app: &mut PeekAppState, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollDown => {
            app.scroll_y = app.scroll_y.saturating_add(3);
        }
        MouseEventKind::ScrollUp => {
            app.scroll_y = app.scroll_y.saturating_sub(3);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let x = mouse.column;
            let y = mouse.row;
            let hit = app
                .hitboxes
                .iter()
                .rev()
                .find(|hb| {
                    x >= hb.rect.x
                        && x < hb.rect.x + hb.rect.width
                        && y >= hb.rect.y
                        && y < hb.rect.y + hb.rect.height
                })
                .map(|hb| hb.target.clone());

            if let Some(target) = hit {
                match target {
                    HitTarget::SelectScope(sc) => app.switch_scope(sc),
                    HitTarget::SelectFileIndex(idx) => {
                        if let Some(entry) = app.active_scope_files().get(idx).cloned() {
                            app.follow_chat = false;
                            app.select_document(entry.path, true);
                        }
                    }
                    HitTarget::ToggleSidebar => {
                        app.sidebar_open = !app.sidebar_open;
                        app.layout_dirty = true;
                        app.raster_engine.invalidate_sent_placements();
                    }
                    HitTarget::ToggleAllDetails => app.toggle_all_details(),
                    HitTarget::ToggleFollowChat => {
                        app.follow_chat = !app.follow_chat;
                    }
                    HitTarget::ZoomReset => {
                        app.zoom_pct = 100;
                        app.layout_dirty = true;
                        app.raster_engine.invalidate_sent_placements();
                    }
                    HitTarget::OpenInEditor => app.open_current_in_editor(),
                    HitTarget::OpenDefaultViewer => app.open_current_in_default_viewer(),
                    HitTarget::RefreshAll => {
                        app.raster_engine.clear_cached_rasters();
                        app.refresh_discovered_files(false, true);
                        if let Some(sel) = app.selected_path.clone() {
                            app.select_document(sel, false);
                        }
                    }
                    HitTarget::CloseViewer => app.should_quit = true,
                    HitTarget::ToggleDetailsBlock(id) => {
                        if !app.expanded_details.remove(&id) {
                            app.expanded_details.insert(id);
                        }
                        app.layout_dirty = true;
                        app.raster_engine.invalidate_sent_placements();
                    }
                }
            }
        }
        _ => {}
    }
}

fn spawn_preferred_editor(path: &Path) -> Option<&'static str> {
    for (cmd_name, label) in [("zed", "Zed"), ("code", "VS Code")] {
        if Command::new(cmd_name)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
        {
            return Some(label);
        }
    }

    #[cfg(target_os = "macos")]
    {
        if Path::new("/Applications/Zed.app").exists()
            && Command::new("/usr/bin/open")
                .args(["-a", "Zed"])
                .arg(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .is_ok()
        {
            return Some("Zed");
        }
    }

    None
}

fn spawn_system_default_opener(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        Command::new("/usr/bin/open")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(())
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(())
    }
}

fn wrap_styled_spans(
    spans: &[StyledInlineSpan],
    max_width: usize,
    palette: &RexThemePalette,
    override_fg: Option<ratatui::style::Color>,
) -> Vec<Vec<Span<'static>>> {
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut current_line: Vec<Span<'static>> = Vec::new();
    let mut current_width: usize = 0;

    for item in spans {
        let style = to_ratatui_style(&item.style, palette, override_fg);
        for (seg_idx, hard_part) in item.text.split('\n').enumerate() {
            if seg_idx > 0 {
                lines.push(std::mem::take(&mut current_line));
                current_width = 0;
            }
            for token in split_preserving_spaces(hard_part) {
                let token_w = UnicodeWidthStr::width(token);
                if current_width > 0 && current_width + token_w > max_width {
                    lines.push(std::mem::take(&mut current_line));
                    current_width = 0;
                    let trimmed = token.trim_start();
                    if !trimmed.is_empty() {
                        current_width = UnicodeWidthStr::width(trimmed);
                        current_line.push(Span::styled(trimmed.to_string(), style));
                    }
                } else {
                    current_width += token_w;
                    current_line.push(Span::styled(token.to_string(), style));
                }
            }
        }
    }

    if !current_line.is_empty() || lines.is_empty() {
        lines.push(current_line);
    }
    lines
}

fn split_preserving_spaces(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_space = false;
    for (idx, ch) in text.char_indices() {
        let is_sp = ch.is_whitespace();
        if idx > 0 && is_sp != in_space {
            parts.push(&text[start..idx]);
            start = idx;
        }
        in_space = is_sp;
    }
    if start < text.len() {
        parts.push(&text[start..]);
    }
    parts
}

fn to_ratatui_style(
    inline: &InlineStyle,
    palette: &RexThemePalette,
    override_fg: Option<ratatui::style::Color>,
) -> Style {
    let mut style = Style::default().fg(override_fg.unwrap_or(palette.fg));
    if inline.inline_code {
        style = style.fg(palette.code_fg).bg(palette.code_bg);
    } else if inline.link_url.is_some() {
        style = style.fg(palette.link).add_modifier(Modifier::UNDERLINED);
    }
    if inline.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if inline.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if inline.strikethrough {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    style
}

fn format_table_lines(
    headers: &[String],
    rows: &[Vec<String>],
    max_width: usize,
    palette: &RexThemePalette,
) -> Vec<Line<'static>> {
    let col_count = headers
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    if col_count == 0 {
        return Vec::new();
    }

    let mut widths = vec![3usize; col_count];
    for (i, h) in headers.iter().enumerate() {
        widths[i] = widths[i].max(UnicodeWidthStr::width(h.as_str()));
    }
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(UnicodeWidthStr::width(cell.as_str()));
        }
    }

    let overhead = col_count * 3 + 3;
    let avail_cells = max_width.saturating_sub(overhead).max(col_count * 4);
    let sum_widths: usize = widths.iter().sum();
    if sum_widths > avail_cells {
        for w in &mut widths {
            *w = ((*w * avail_cells) / sum_widths).max(4);
        }
    }

    let border_style = Style::default().fg(palette.border);
    let build_border = |left: &str, mid: &str, right: &str| -> Line<'static> {
        let segs: Vec<String> = widths.iter().map(|w| "─".repeat(*w + 2)).collect();
        Line::from(Span::styled(
            format!("  {left}{}{right}", segs.join(mid)),
            border_style,
        ))
    };

    let build_row = |cells: &[String], is_header: bool| -> Line<'static> {
        let mut spans = vec![Span::styled("  │", border_style)];
        for (i, col_w) in widths.iter().enumerate() {
            let raw = cells.get(i).map(String::as_str).unwrap_or("");
            let truncated: String = raw.chars().take(*col_w).collect();
            let pad = col_w.saturating_sub(UnicodeWidthStr::width(truncated.as_str()));
            let cell_style = if is_header {
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.fg)
            };
            spans.push(Span::styled(
                format!(" {truncated}{} ", " ".repeat(pad)),
                cell_style,
            ));
            spans.push(Span::styled("│", border_style));
        }
        Line::from(spans)
    };

    let mut out = Vec::new();
    out.push(build_border("┌", "┬", "┐"));
    if !headers.is_empty() {
        out.push(build_row(headers, true));
        out.push(build_border("├", "┼", "┤"));
    }
    for row in rows {
        out.push(build_row(row, false));
    }
    out.push(build_border("└", "┴", "┘"));
    out
}

fn read_mtime_nanos(path: &Path) -> u128 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
