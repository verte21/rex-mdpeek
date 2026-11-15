mod diagram_renderer;
mod discovery;
mod document;
mod installer;
mod rex_bridge;
mod theme;
mod ui;

use std::env;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Result, bail};

use crate::installer::{install_rex_extension, uninstall_rex_extension};
use crate::rex_bridge::resolve_rex_binary;
use crate::ui::{PeekLaunchOptions, run_peek_tui};

fn main() -> Result<()> {
    let raw_args: Vec<String> = env::args().skip(1).collect();
    if raw_args.is_empty() {
        return launch_ui(&[]);
    }

    match raw_args[0].as_str() {
        "ui" => launch_ui(&raw_args[1..]),
        "open" => open_via_rex_action(&raw_args[1..]),
        "install" => install_rex_extension(),
        "uninstall" => uninstall_rex_extension(),
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        "-V" | "--version" | "version" => {
            println!("rex-mdpeek {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        other if !other.starts_with('-') => {
            let opts = PeekLaunchOptions {
                session_id: None,
                workspace_cwd: env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                mode: "float".to_string(),
                source_block: None,
                explicit_path: Some(PathBuf::from(other)),
            };
            run_peek_tui(opts)
        }
        _ => launch_ui(&raw_args),
    }
}

fn launch_ui(args: &[String]) -> Result<()> {
    let mut session_id: Option<String> = None;
    let mut workspace_cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut mode = "float".to_string();
    let mut source_block: Option<String> = None;
    let mut explicit_path: Option<PathBuf> = None;

    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--session" => {
                idx += 1;
                if let Some(val) = args.get(idx) {
                    session_id = Some(val.clone());
                }
            }
            "--cwd" => {
                idx += 1;
                if let Some(val) = args.get(idx) {
                    workspace_cwd = PathBuf::from(val);
                }
            }
            "--mode" => {
                idx += 1;
                if let Some(val) = args.get(idx) {
                    mode = val.clone();
                }
            }
            "--source-block" => {
                idx += 1;
                if let Some(val) = args.get(idx) {
                    source_block = Some(val.clone());
                }
            }
            "--path" => {
                idx += 1;
                if let Some(val) = args.get(idx) {
                    explicit_path = Some(PathBuf::from(val));
                }
            }
            other => bail!("Unknown option: {other}"),
        }
        idx += 1;
    }

    run_peek_tui(PeekLaunchOptions {
        session_id,
        workspace_cwd,
        mode,
        source_block,
        explicit_path,
    })
}

fn open_via_rex_action(args: &[String]) -> Result<()> {
    let mut dock = false;
    let mut path_arg: Option<PathBuf> = None;

    for token in args {
        if token == "--dock" {
            dock = true;
        } else if !token.starts_with('-') {
            path_arg = Some(PathBuf::from(token));
        }
    }

    let action_name = if dock { "mdpeek_dock" } else { "mdpeek_toggle" };
    let rex_bin = resolve_rex_binary();
    let mut cmd = Command::new(rex_bin);
    cmd.args(["do", action_name]);

    if let Some(p) = path_arg {
        let abs = if p.is_absolute() {
            p
        } else {
            env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(p)
        };
        cmd.arg(format!("path={}", abs.display()));
    }

    let status = cmd.status()?;
    if !status.success() {
        bail!("rex do {action_name} exited with non-zero status");
    }
    Ok(())
}

fn print_usage() {
    println!(
        "rex-mdpeek — Cross-Platform Markdown & Pele Diagram Viewer for Rex (Kitty Graphics)

Usage:
  rex-mdpeek [path.md]
  rex-mdpeek ui [--mode float|dock] [--session <id>] [--source-block <id>] [--cwd <dir>] [--path <file.md>]
  rex-mdpeek open [file.md] [--dock]
  rex-mdpeek install
  rex-mdpeek uninstall"
    );
}
