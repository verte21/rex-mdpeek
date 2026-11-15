use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use flate2::Compression;
use flate2::write::ZlibEncoder;
use image::imageops::{self, FilterType};
use image::{Rgba, RgbaImage};
use resvg::tiny_skia::{Color as SkiaColor, Pixmap, Transform};
use resvg::usvg::{Options as UsvgOptions, Tree as UsvgTree, fontdb};
use sha2::{Digest, Sha256};

use crate::discovery::home_directory;
use crate::document::DiagramAssetSource;
use crate::theme::{RexThemePalette, format_rgb_hex};

const KITTY_DIAGRAM_ID_BASE: u32 = 5100;
const MAX_UPLOADED_GPU_TEXTURES: usize = 32;

const EMBEDDED_ROBOTO_REGULAR: &[u8] = include_bytes!("../assets/fonts/Roboto-Regular.ttf");
const EMBEDDED_ROBOTO_BOLD: &[u8] = include_bytes!("../assets/fonts/Roboto-Bold.ttf");
const EMBEDDED_FIRA_CODE_REGULAR: &[u8] = include_bytes!("../assets/fonts/FiraCode-Regular.ttf");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCellMetrics {
    pub cell_px_width: u32,
    pub cell_px_height: u32,
}

impl TerminalCellMetrics {
    pub fn detect() -> Self {
        if let Ok(ws) = crossterm::terminal::window_size()
            && ws.columns > 0
            && ws.rows > 0
            && ws.width > 0
            && ws.height > 0
        {
            let w = ((ws.width as u32) / (ws.columns as u32)).clamp(8, 48);
            let h = ((ws.height as u32) / (ws.rows as u32)).clamp(16, 96);
            return Self {
                cell_px_width: w,
                cell_px_height: h,
            };
        }
        Self {
            cell_px_width: 20,
            cell_px_height: 40,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RasterizedDiagram {
    pub image_id: u32,
    pub compressed_rgba_b64: String,
    pub canvas_px_w: u32,
    pub canvas_px_h: u32,
    pub cell_cols: u16,
    pub cell_rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleKittyPlacement {
    pub image_id: u32,
    pub placement_id: u32,
    pub asset_cache_key: String,
    pub screen_col_1idx: u16,
    pub screen_row_1idx: u16,
    pub cell_cols: u16,
    pub total_cell_rows: u16,
    pub visible_row_offset: u16,
    pub visible_cell_rows: u16,
}

pub struct DiagramRasterEngine {
    fontdb: Arc<fontdb::Database>,
    next_image_id: u32,
    raster_cache: HashMap<String, RasterizedDiagram>,
    uploaded_image_ids: VecDeque<u32>,
    active_placements: HashSet<(u32, u32)>,
    last_emitted_signature: String,
    force_resync: bool,
    purge_gpu_on_next_emit: bool,
}

impl DiagramRasterEngine {
    pub fn new() -> Self {
        let mut db = fontdb::Database::new();
        db.load_font_data(EMBEDDED_ROBOTO_REGULAR.to_vec());
        db.load_font_data(EMBEDDED_ROBOTO_BOLD.to_vec());
        db.load_font_data(EMBEDDED_FIRA_CODE_REGULAR.to_vec());
        db.set_sans_serif_family("Roboto");
        db.set_monospace_family("Fira Code");
        db.set_serif_family("Roboto");

        Self {
            fontdb: Arc::new(db),
            next_image_id: KITTY_DIAGRAM_ID_BASE,
            raster_cache: HashMap::new(),
            uploaded_image_ids: VecDeque::new(),
            active_placements: HashSet::new(),
            last_emitted_signature: String::new(),
            force_resync: true,
            purge_gpu_on_next_emit: false,
        }
    }

    pub fn invalidate_sent_placements(&mut self) {
        self.force_resync = true;
    }

    pub fn clear_cached_rasters(&mut self) {
        self.raster_cache.clear();
        self.force_resync = true;
        self.purge_gpu_on_next_emit = true;
    }

    pub fn prepare_diagram_layout(
        &mut self,
        asset: &DiagramAssetSource,
        available_cols: u16,
        zoom_pct: u16,
        metrics: TerminalCellMetrics,
        palette: &RexThemePalette,
    ) -> Option<(String, u32, u16, u16)> {
        let avail = available_cols.max(12);
        let asset_key = compute_asset_cache_key(asset, avail, zoom_pct, metrics, palette)?;

        if !self.raster_cache.contains_key(&asset_key) {
            let assigned_id = self.next_image_id;
            self.next_image_id = self.next_image_id.wrapping_add(1).max(KITTY_DIAGRAM_ID_BASE);
            let rasterized =
                self.rasterize_asset(asset, assigned_id, avail, zoom_pct, metrics, palette)?;
            self.raster_cache.insert(asset_key.clone(), rasterized);
        }

        let entry = self.raster_cache.get(&asset_key)?;
        Some((asset_key, entry.image_id, entry.cell_cols, entry.cell_rows))
    }

    pub fn emit_visible_placements<W: Write>(
        &mut self,
        writer: &mut W,
        placements: &[VisibleKittyPlacement],
    ) -> Result<()> {
        let frame_signature = placements
            .iter()
            .map(|p| {
                format!(
                    "{}:{}:{}:{}:{}:{}:{}:{}",
                    p.image_id,
                    p.placement_id,
                    p.screen_col_1idx,
                    p.screen_row_1idx,
                    p.cell_cols,
                    p.total_cell_rows,
                    p.visible_row_offset,
                    p.visible_cell_rows
                )
            })
            .collect::<Vec<_>>()
            .join("|");

        if !self.force_resync
            && !self.purge_gpu_on_next_emit
            && frame_signature == self.last_emitted_signature
        {
            return Ok(());
        }

        if self.purge_gpu_on_next_emit {
            for old_id in self.uploaded_image_ids.drain(..) {
                write!(writer, "\x1b_Ga=d,d=I,i={old_id},q=2\x1b\\")?;
            }
            write!(writer, "\x1b_Ga=d,d=A,q=2\x1b\\")?;
            self.active_placements.clear();
            self.purge_gpu_on_next_emit = false;
        }

        let mut next_active: HashSet<(u32, u32)> = HashSet::with_capacity(placements.len());
        let mut needed_image_ids: HashSet<u32> = HashSet::with_capacity(placements.len());
        for p in placements {
            if p.visible_cell_rows > 0 && p.cell_cols > 0 {
                next_active.insert((p.image_id, p.placement_id));
                needed_image_ids.insert(p.image_id);
            }
        }

        for &(stale_img_id, stale_plc_id) in self.active_placements.difference(&next_active) {
            write!(
                writer,
                "\x1b_Ga=d,d=i,i={stale_img_id},p={stale_plc_id},q=2\x1b\\"
            )?;
        }

        for placement in placements {
            if placement.visible_cell_rows == 0 || placement.cell_cols == 0 {
                continue;
            }
            let Some(raster) = self.raster_cache.get(&placement.asset_cache_key) else {
                continue;
            };

            if !self.uploaded_image_ids.contains(&raster.image_id) {
                while self.uploaded_image_ids.len() >= MAX_UPLOADED_GPU_TEXTURES {
                    if let Some(pos) = self
                        .uploaded_image_ids
                        .iter()
                        .position(|id| !needed_image_ids.contains(id))
                        && let Some(evicted_id) = self.uploaded_image_ids.remove(pos)
                    {
                        write!(writer, "\x1b_Ga=d,d=I,i={evicted_id},q=2\x1b\\")?;
                    } else {
                        break;
                    }
                }
                write_kitty_zlib_upload(
                    writer,
                    raster.image_id,
                    raster.canvas_px_w,
                    raster.canvas_px_h,
                    &raster.compressed_rgba_b64,
                )?;
                self.uploaded_image_ids.push_back(raster.image_id);
            }

            let (src_y, src_h) = compute_source_vertical_slice(
                raster.canvas_px_h,
                placement.total_cell_rows,
                placement.visible_row_offset,
                placement.visible_cell_rows,
            );
            let src_w = raster.canvas_px_w;

            write!(
                writer,
                "\x1b[{row};{col}H\x1b_Ga=p,i={img_id},p={plc_id},x=0,y={src_y},w={src_w},h={src_h},c={cols},r={rows},C=1,q=2\x1b\\",
                row = placement.screen_row_1idx,
                col = placement.screen_col_1idx,
                img_id = raster.image_id,
                plc_id = placement.placement_id,
                cols = placement.cell_cols,
                rows = placement.visible_cell_rows,
            )?;
        }

        writer.flush()?;
        self.active_placements = next_active;
        self.last_emitted_signature = frame_signature;
        self.force_resync = false;
        Ok(())
    }

    pub fn purge_all_kitty_textures<W: Write>(&mut self, writer: &mut W) -> Result<()> {
        for uploaded_id in self.uploaded_image_ids.drain(..) {
            write!(writer, "\x1b_Ga=d,d=I,i={uploaded_id},q=2\x1b\\")?;
        }
        write!(writer, "\x1b_Ga=d,d=A,q=2\x1b\\")?;
        self.active_placements.clear();
        writer.flush()?;
        Ok(())
    }

    fn rasterize_asset(
        &self,
        asset: &DiagramAssetSource,
        image_id: u32,
        available_cols: u16,
        zoom_pct: u16,
        metrics: TerminalCellMetrics,
        palette: &RexThemePalette,
    ) -> Option<RasterizedDiagram> {
        match asset {
            DiagramAssetSource::File(path) => {
                let ext = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if ext == "svg" {
                    let raw_svg = fs::read_to_string(path).ok()?;
                    self.rasterize_svg_markup(
                        &raw_svg,
                        path.parent(),
                        image_id,
                        available_cols,
                        zoom_pct,
                        metrics,
                        palette,
                    )
                } else {
                    self.rasterize_bitmap_file(
                        path,
                        image_id,
                        available_cols,
                        zoom_pct,
                        metrics,
                        palette,
                    )
                }
            }
            DiagramAssetSource::InlineMermaid(mermaid_source) => {
                let svg_path = render_inline_mermaid_via_pele_skill(mermaid_source, palette)?;
                let raw_svg = fs::read_to_string(&svg_path).ok()?;
                self.rasterize_svg_markup(
                    &raw_svg,
                    svg_path.parent(),
                    image_id,
                    available_cols,
                    zoom_pct,
                    metrics,
                    palette,
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rasterize_svg_markup(
        &self,
        raw_svg: &str,
        resources_dir: Option<&Path>,
        image_id: u32,
        available_cols: u16,
        zoom_pct: u16,
        metrics: TerminalCellMetrics,
        palette: &RexThemePalette,
    ) -> Option<RasterizedDiagram> {
        let prepared_svg = normalize_svg_for_resvg(raw_svg, palette);

        let mut options = UsvgOptions {
            fontdb: Arc::clone(&self.fontdb),
            font_family: "Roboto".to_string(),
            font_size: 16.0,
            ..UsvgOptions::default()
        };
        if let Some(dir) = resources_dir {
            options.resources_dir = Some(dir.to_path_buf());
        }

        let tree = UsvgTree::from_str(&prepared_svg, &options).ok()?;
        let natural_w = tree.size().width().max(1.0);
        let natural_h = tree.size().height().max(1.0);

        let retina_scale = ((metrics.cell_px_height as f32) / 22.0).clamp(1.0, 3.0);
        let desired_px_w = natural_w * retina_scale * (zoom_pct as f32 / 100.0);
        let max_px_w = (available_cols as u32 * metrics.cell_px_width).max(120) as f32;
        let render_scale = (desired_px_w.min(max_px_w) / natural_w).max(0.1);

        let target_px_w = (natural_w * render_scale).round().clamp(16.0, 4096.0) as u32;
        let target_px_h = (natural_h * render_scale).round().clamp(16.0, 4096.0) as u32;

        let cell_cols = ((target_px_w as f32) / (metrics.cell_px_width as f32))
            .ceil()
            .clamp(1.0, available_cols as f32) as u16;
        let cell_rows = ((target_px_h as f32) / (metrics.cell_px_height as f32))
            .ceil()
            .clamp(2.0, 120.0) as u16;

        let canvas_px_w = (cell_cols as u32 * metrics.cell_px_width).max(target_px_w);
        let canvas_px_h = (cell_rows as u32 * metrics.cell_px_height).max(target_px_h);

        let mut pixmap = Pixmap::new(canvas_px_w, canvas_px_h)?;
        let (bg_r, bg_g, bg_b) = palette.bg_rgb;
        pixmap.fill(SkiaColor::from_rgba8(bg_r, bg_g, bg_b, 255));

        let offset_y = ((canvas_px_h.saturating_sub(target_px_h)) as f32) * 0.5;
        let transform =
            Transform::from_scale(render_scale, render_scale).post_translate(0.0, offset_y);
        resvg::render(&tree, transform, &mut pixmap.as_mut());

        let compressed_rgba_b64 = compress_rgba_zlib_base64(pixmap.data()).ok()?;
        Some(RasterizedDiagram {
            image_id,
            compressed_rgba_b64,
            canvas_px_w,
            canvas_px_h,
            cell_cols,
            cell_rows,
        })
    }

    fn rasterize_bitmap_file(
        &self,
        path: &Path,
        image_id: u32,
        available_cols: u16,
        zoom_pct: u16,
        metrics: TerminalCellMetrics,
        palette: &RexThemePalette,
    ) -> Option<RasterizedDiagram> {
        let source_img = image::open(path).ok()?.into_rgba8();
        let src_w = source_img.width().max(1) as f32;
        let src_h = source_img.height().max(1) as f32;

        let desired_px_w = src_w * (zoom_pct as f32 / 100.0);
        let max_px_w = (available_cols as u32 * metrics.cell_px_width).max(120) as f32;
        let scale = (desired_px_w.min(max_px_w) / src_w).clamp(0.05, 4.0);

        let target_px_w = (src_w * scale).round().clamp(16.0, 4096.0) as u32;
        let target_px_h = (src_h * scale).round().clamp(16.0, 4096.0) as u32;

        let resized =
            imageops::resize(&source_img, target_px_w, target_px_h, FilterType::Lanczos3);
        let cell_cols = ((target_px_w as f32) / (metrics.cell_px_width as f32))
            .ceil()
            .clamp(1.0, available_cols as f32) as u16;
        let cell_rows = ((target_px_h as f32) / (metrics.cell_px_height as f32))
            .ceil()
            .clamp(2.0, 120.0) as u16;

        let canvas_px_w = (cell_cols as u32 * metrics.cell_px_width).max(target_px_w);
        let canvas_px_h = (cell_rows as u32 * metrics.cell_px_height).max(target_px_h);
        let (bg_r, bg_g, bg_b) = palette.bg_rgb;
        let mut canvas =
            RgbaImage::from_pixel(canvas_px_w, canvas_px_h, Rgba([bg_r, bg_g, bg_b, 255]));
        let offset_y = (canvas_px_h.saturating_sub(target_px_h)) / 2;
        imageops::overlay(&mut canvas, &resized, 0, offset_y as i64);

        let compressed_rgba_b64 = compress_rgba_zlib_base64(canvas.as_raw()).ok()?;
        Some(RasterizedDiagram {
            image_id,
            compressed_rgba_b64,
            canvas_px_w,
            canvas_px_h,
            cell_cols,
            cell_rows,
        })
    }
}

fn compress_rgba_zlib_base64(raw_rgba: &[u8]) -> Result<String> {
    let mut encoder = ZlibEncoder::new(
        Vec::with_capacity((raw_rgba.len() / 12).max(1024)),
        Compression::fast(),
    );
    encoder.write_all(raw_rgba)?;
    let compressed = encoder.finish()?;
    Ok(BASE64.encode(&compressed))
}

fn write_kitty_zlib_upload<W: Write>(
    writer: &mut W,
    image_id: u32,
    px_w: u32,
    px_h: u32,
    encoded_b64: &str,
) -> Result<()> {
    let bytes = encoded_b64.as_bytes();
    let total_chunks = bytes.len().div_ceil(4096).max(1);
    for (idx, chunk_bytes) in bytes.chunks(4096).enumerate() {
        let chunk_str = std::str::from_utf8(chunk_bytes).unwrap_or("");
        let more = usize::from(idx + 1 < total_chunks);
        if idx == 0 {
            write!(
                writer,
                "\x1b_Ga=t,f=32,o=z,s={px_w},v={px_h},i={image_id},q=2,m={more};{chunk_str}\x1b\\"
            )?;
        } else {
            write!(writer, "\x1b_Gm={more},q=2;{chunk_str}\x1b\\")?;
        }
    }
    Ok(())
}

fn compute_source_vertical_slice(
    canvas_px_h: u32,
    total_cell_rows: u16,
    visible_row_offset: u16,
    visible_cell_rows: u16,
) -> (u32, u32) {
    let total_h = canvas_px_h.max(1) as u64;
    let total_rows = total_cell_rows.max(1) as u64;
    if visible_row_offset == 0 && visible_cell_rows >= total_cell_rows {
        return (0, canvas_px_h.max(1));
    }

    let start_row = (visible_row_offset as u64).min(total_rows.saturating_sub(1));
    let end_row =
        ((visible_row_offset as u64) + (visible_cell_rows as u64)).clamp(start_row + 1, total_rows);

    let src_y = ((start_row * total_h) / total_rows).min(total_h - 1) as u32;
    let y_end = ((end_row * total_h) / total_rows).clamp((src_y as u64) + 1, total_h) as u32;
    (src_y, (y_end - src_y).max(1))
}

fn compute_asset_cache_key(
    asset: &DiagramAssetSource,
    available_cols: u16,
    zoom_pct: u16,
    metrics: TerminalCellMetrics,
    palette: &RexThemePalette,
) -> Option<String> {
    match asset {
        DiagramAssetSource::File(path) => {
            if !path.is_file() {
                return None;
            }
            let meta = fs::metadata(path).ok()?;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            Some(format!(
                "file:{}:{}:{}:{}:{}:{}:{}",
                path.display(),
                mtime,
                available_cols,
                zoom_pct,
                metrics.cell_px_width,
                metrics.cell_px_height,
                palette.theme_id
            ))
        }
        DiagramAssetSource::InlineMermaid(source) => {
            let mut hasher = Sha256::new();
            hasher.update(source.as_bytes());
            let digest = format!("{:x}", hasher.finalize());
            Some(format!(
                "mmd:{}:{}:{}:{}:{}:{}",
                &digest[..16],
                available_cols,
                zoom_pct,
                metrics.cell_px_width,
                metrics.cell_px_height,
                palette.theme_id
            ))
        }
    }
}

fn render_inline_mermaid_via_pele_skill(
    mermaid_source: &str,
    palette: &RexThemePalette,
) -> Option<PathBuf> {
    let cache_dir = env::temp_dir().join("diagrams").join(".pele-cache");
    fs::create_dir_all(&cache_dir).ok()?;

    let theme_mode = if palette.is_dark { "dark" } else { "light" };
    let mut hasher = Sha256::new();
    hasher.update(theme_mode.as_bytes());
    hasher.update(mermaid_source.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    let cached_svg = cache_dir.join(format!("{}-{theme_mode}.svg", &digest[..20]));

    if cached_svg.is_file() {
        return Some(cached_svg);
    }

    let script_path = home_directory()
        .join(".agents")
        .join("skills")
        .join("pele")
        .join("scripts")
        .join("render-pele.mjs");
    if !script_path.is_file() {
        return None;
    }

    let node_bin = resolve_node_binary();
    let mut child = Command::new(node_bin)
        .arg(&script_path)
        .arg("-")
        .arg(&cached_svg)
        .arg("--theme")
        .arg(theme_mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(mermaid_source.as_bytes());
    }
    let status = child.wait().ok()?;
    (status.success() && cached_svg.is_file()).then_some(cached_svg)
}

fn resolve_node_binary() -> String {
    if let Ok(explicit) = env::var("NODE_BINARY")
        && !explicit.is_empty()
        && Path::new(&explicit).is_file()
    {
        return explicit;
    }

    let exe_name = format!("node{}", env::consts::EXE_SUFFIX);
    let home = home_directory();

    if let Some(nvm_symlink) = env::var_os("NVM_SYMLINK") {
        let candidate = PathBuf::from(nvm_symlink).join(&exe_name);
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }

    let nvm_dir = home.join(".nvm").join("versions").join("node");
    if let Ok(entries) = fs::read_dir(nvm_dir) {
        let mut versions: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        versions.sort();
        if let Some(latest) = versions.pop() {
            let bin = latest.join("bin").join(&exe_name);
            if bin.is_file() {
                return bin.to_string_lossy().into_owned();
            }
        }
    }

    for rel in [".volta/bin", ".fnm/current/bin"] {
        let candidate = home.join(rel).join(&exe_name);
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }

    if let Some(program_files) = env::var_os("ProgramFiles") {
        let candidate = PathBuf::from(program_files).join("nodejs").join("node.exe");
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }

    for candidate in ["/opt/homebrew/bin/node", "/usr/local/bin/node", "/usr/bin/node"] {
        if Path::new(candidate).is_file() {
            return candidate.to_string();
        }
    }

    "node".to_string()
}

fn normalize_svg_for_resvg(raw_svg: &str, palette: &RexThemePalette) -> String {
    if !raw_svg.contains("var(--") {
        return raw_svg
            .replace("style=\"rx:6px\"", "rx=\"6\" ry=\"6\"")
            .replace("style=\"rx:4px\"", "rx=\"4\" ry=\"4\"");
    }

    let bg_hex = format_rgb_hex(palette.bg_rgb);
    let mut vars: HashMap<String, String> = HashMap::from([
        ("--pele-bg".to_string(), bg_hex),
        ("--pele-fg".to_string(), palette.fg_hex.clone()),
        ("--pele-muted".to_string(), palette.muted_hex.clone()),
        ("--pele-line".to_string(), palette.fg_hex.clone()),
        ("--pele-surface".to_string(), palette.surface_hex.clone()),
        (
            "--pele-surface-alt".to_string(),
            palette.surface_alt_hex.clone(),
        ),
        ("--pele-border".to_string(), palette.border_hex.clone()),
        ("--pele-accent".to_string(), palette.accent_hex.clone()),
        ("--pele-radius".to_string(), "6px".to_string()),
        ("--pele-font".to_string(), "Roboto, sans-serif".to_string()),
        (
            "--pele-font-mono".to_string(),
            "Fira Code, monospace".to_string(),
        ),
        ("--pele-title-weight".to_string(), "bold".to_string()),
    ]);

    if let Some(style_start) = raw_svg.find("style=\"") {
        let after = &raw_svg[style_start + 7..];
        if let Some(style_end) = after.find('"') {
            for decl in after[..style_end].split(';') {
                if let Some((k, v)) = decl.split_once(':') {
                    let key = k.trim();
                    if key.starts_with("--") && !vars.contains_key(key) {
                        vars.insert(key.to_string(), v.trim().to_string());
                    }
                }
            }
        }
    }

    let mut current = raw_svg.to_string();
    for _ in 0..8 {
        let next = replace_css_var_calls(&current, &vars);
        if next == current {
            break;
        }
        current = next;
    }

    current
        .replace("style=\"rx:6px\"", "rx=\"6\" ry=\"6\"")
        .replace("style=\"rx:4px\"", "rx=\"4\" ry=\"4\"")
}

fn replace_css_var_calls(input: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find("var(--") {
        out.push_str(&rest[..pos]);
        let after_var = &rest[pos + 4..];
        if let Some(close_idx) = find_matching_paren(after_var) {
            let inner = &after_var[..close_idx];
            let (var_name, fallback) = match inner.split_once(',') {
                Some((n, fb)) => (n.trim(), Some(fb.trim())),
                None => (inner.trim(), None),
            };
            if let Some(resolved) = vars.get(var_name) {
                out.push_str(resolved);
            } else if let Some(fb) = fallback {
                out.push_str(fb);
            } else {
                out.push_str("currentColor");
            }
            rest = &after_var[close_idx + 1..];
        } else {
            out.push_str("var(--");
            rest = after_var;
        }
    }
    out.push_str(rest);
    out
}

fn find_matching_paren(text: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (idx, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}
