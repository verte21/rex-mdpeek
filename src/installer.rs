use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

use crate::discovery::home_directory;
use crate::rex_bridge::resolve_rex_binary;

pub const EMBEDDED_MDPEEK_LUA: &str = include_str!("../lua/mdpeek.lua");

fn rex_config_directory() -> PathBuf {
    if let Ok(explicit) = env::var("REX_CONFIG_DIR")
        && !explicit.is_empty()
    {
        return PathBuf::from(explicit);
    }
    if let Ok(xdg_config) = env::var("XDG_CONFIG_HOME")
        && !xdg_config.is_empty()
    {
        return PathBuf::from(xdg_config).join("rex");
    }
    let home = home_directory();
    let dot_config_rex = home.join(".config").join("rex");
    #[cfg(windows)]
    {
        if dot_config_rex.exists() {
            return dot_config_rex;
        }
        if let Some(appdata) = env::var_os("APPDATA") {
            let appdata_rex = PathBuf::from(appdata).join("rex");
            if appdata_rex.exists() {
                return appdata_rex;
            }
        }
    }
    dot_config_rex
}

fn target_binary_directory() -> PathBuf {
    let home = home_directory();
    #[cfg(windows)]
    {
        let cargo_bin = home.join(".cargo").join("bin");
        if cargo_bin.is_dir() {
            return cargo_bin;
        }
    }
    home.join(".local").join("bin")
}

pub fn install_rex_extension() -> Result<()> {
    let bin_dir = target_binary_directory();
    fs::create_dir_all(&bin_dir)?;

    let exe_name = format!("rex-mdpeek{}", env::consts::EXE_SUFFIX);
    let current_exe = env::current_exe().context("failed to locate running rex-mdpeek binary")?;
    let target_bin = bin_dir.join(&exe_name);
    if current_exe != target_bin {
        let tmp_bin = bin_dir.join(format!(".{exe_name}.tmp"));
        fs::copy(&current_exe, &tmp_bin)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&tmp_bin)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&tmp_bin, perms)?;
        }
        fs::rename(&tmp_bin, &target_bin)?;
    }

    let config_dir = rex_config_directory();
    fs::create_dir_all(&config_dir)?;
    let lua_path = config_dir.join("mdpeek.lua");
    if !lua_path.is_symlink() {
        fs::write(&lua_path, EMBEDDED_MDPEEK_LUA)?;
    }

    let init_lua = config_dir.join("init.lua");
    if init_lua.is_file() {
        let existing = fs::read_to_string(&init_lua)?;
        if !existing.contains("require(\"mdpeek\")") && !existing.contains("require('mdpeek')") {
            let mut updated = existing.trim_end().to_string();
            updated.push_str("\npcall(function() require(\"mdpeek\") end)\n");
            fs::write(&init_lua, updated)?;
        }
    } else {
        fs::write(&init_lua, "pcall(function() require(\"mdpeek\") end)\n")?;
    }

    let rex_bin = resolve_rex_binary();
    let _ = Command::new(&rex_bin)
        .args(["config", "check"])
        .arg(&init_lua)
        .status();
    let _ = Command::new(&rex_bin).args(["config", "reload"]).status();

    println!(
        "Installed {} and registered Rex actions in {}",
        target_bin.display(),
        lua_path.display()
    );
    Ok(())
}

pub fn uninstall_rex_extension() -> Result<()> {
    let config_dir = rex_config_directory();
    let lua_path = config_dir.join("mdpeek.lua");
    if lua_path.exists() {
        let _ = fs::remove_file(&lua_path);
    }

    let init_lua = config_dir.join("init.lua");
    if init_lua.is_file() {
        let existing = fs::read_to_string(&init_lua)?;
        let filtered: Vec<&str> = existing
            .lines()
            .filter(|l| !l.contains("mdpeek"))
            .collect();
        fs::write(&init_lua, format!("{}\n", filtered.join("\n")))?;
    }

    let rex_bin = resolve_rex_binary();
    let _ = Command::new(&rex_bin).args(["config", "reload"]).status();
    Ok(())
}
