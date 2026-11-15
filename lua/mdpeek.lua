local IS_WINDOWS = package.config:sub(1, 1) == "\\"
local SHELL_FLAVOR = "com.superlogical.terminal.shell"
local FLOAT_LABEL = "mdpeek-float"
local DOCK_LABEL = "mdpeek-dock"

local OVERLAY_LABELS = {
  [FLOAT_LABEL] = true,
  [DOCK_LABEL] = true,
  ["omnidiff-float"] = true,
  ["omnidiff-dock"] = true,
  ["lazygit-float"] = true,
  ["scratch-float"] = true,
  ["btop-float"] = true,
}

local function file_exists(path)
  if not path or path == "" then
    return false
  end
  local f = io.open(path, "rb")
  if f then
    f:close()
    return true
  end
  return false
end

local function resolve_mdpeek_binary()
  local explicit = os.getenv("REX_MDPEEK_BIN")
  if file_exists(explicit) then
    return explicit
  end

  local home = os.getenv("HOME") or os.getenv("USERPROFILE") or ""
  local candidates = {}

  if IS_WINDOWS then
    if home ~= "" then
      table.insert(candidates, home .. "\\.cargo\\bin\\rex-mdpeek.exe")
      table.insert(candidates, home .. "\\.local\\bin\\rex-mdpeek.exe")
    end
    local local_appdata = os.getenv("LOCALAPPDATA")
    if local_appdata and local_appdata ~= "" then
      table.insert(candidates, local_appdata .. "\\Programs\\rex-mdpeek\\rex-mdpeek.exe")
    end
  else
    if home ~= "" then
      table.insert(candidates, home .. "/.local/bin/rex-mdpeek")
      table.insert(candidates, home .. "/.cargo/bin/rex-mdpeek")
    end
    table.insert(candidates, "/opt/homebrew/bin/rex-mdpeek")
    table.insert(candidates, "/usr/local/bin/rex-mdpeek")
  end

  for _, candidate in ipairs(candidates) do
    if file_exists(candidate) then
      return candidate
    end
  end

  return IS_WINDOWS and "rex-mdpeek.exe" or "rex-mdpeek"
end

local function resolve_active_window(view)
  if not view or not view.windows then
    return nil
  end
  for _, w in ipairs(view.windows) do
    if w.window_id == view.active_window_id then
      return w
    end
  end
  return view.windows[1]
end

local function resolve_block_cwd(block_id)
  if not block_id or block_id == "" then
    return nil
  end
  local proc = rex.block.call("com.superlogical.terminal", "process", {
    block_id = block_id,
  })
  if proc then
    if proc.foreground and proc.foreground.cwd and proc.foreground.cwd ~= "" then
      return proc.foreground.cwd
    end
    if proc.ancestors then
      for _, anc in ipairs(proc.ancestors) do
        if anc.cwd and anc.cwd ~= "" then
          return anc.cwd
        end
      end
    end
    if proc.home and proc.home ~= "" then
      return proc.home
    end
  end
  local pwd_res = rex.block.call("com.superlogical.terminal", "pwd", {
    block_id = block_id,
  })
  if pwd_res and pwd_res.pwd and pwd_res.pwd ~= "" then
    local win_path = string.match(pwd_res.pwd, "^file://[^/]*/([A-Za-z]:/.*)$")
    if win_path and win_path ~= "" then
      return win_path
    end
    local unix_path = string.match(pwd_res.pwd, "^file://[^/]*(/.*)$")
    if unix_path and unix_path ~= "" then
      return unix_path
    end
  end
  return nil
end

local function find_source_chat_block(win, preferred_block_id)
  if not win or not win.layers then
    return preferred_block_id
  end
  local fallback = nil
  for _, layer in ipairs(win.layers) do
    for _, blk in ipairs(layer.blocks or {}) do
      local lbl = blk.label or ""
      if not OVERLAY_LABELS[lbl] then
        if preferred_block_id and blk.block_id == preferred_block_id then
          return blk.block_id
        end
        if win.focused_block_id and blk.block_id == win.focused_block_id then
          fallback = blk.block_id
        elseif not fallback then
          fallback = blk.block_id
        end
      end
    end
  end
  return fallback or preferred_block_id
end

local function build_mdpeek_command(mode, session_id, src_block, cwd, explicit_path)
  local cmd = { resolve_mdpeek_binary(), "ui", "--mode", mode }
  if session_id and session_id ~= "" then
    table.insert(cmd, "--session")
    table.insert(cmd, session_id)
  end
  if src_block and src_block ~= "" then
    table.insert(cmd, "--source-block")
    table.insert(cmd, src_block)
  end
  if cwd and cwd ~= "" then
    table.insert(cmd, "--cwd")
    table.insert(cmd, cwd)
  end
  if explicit_path then
    table.insert(cmd, "--path")
    table.insert(cmd, explicit_path)
  end
  return cmd
end

rex.action{
  name = "mdpeek_toggle",
  title = "Markdown Peek: Toggle Floating Diagram & Doc Viewer",
  description = "Peek Markdown files and Pele SVG diagrams mentioned in the active chat terminal inside a floating Kitty-graphics overlay.",
  category = "View",
  keywords = "markdown md peek diagram mermaid pele svg preview chat float",
  args = {
    path = "string?",
    cwd = "string?",
  },
  run = function(ctx, args)
    local view, err = rex.session.view{}
    if not view then
      error(err or "failed to inspect Rex session", 0)
    end
    local win = resolve_active_window(view)
    if not win then
      error("no active window in current session", 0)
    end

    local explicit_path = args and args.path and args.path ~= "" and args.path or nil

    for _, layer in ipairs(win.layers or {}) do
      if layer.kind ~= "tiled" then
        for _, blk in ipairs(layer.blocks or {}) do
          if blk.label == FLOAT_LABEL then
            local _, close_err = rex.session.close_layer{
              layer_id = layer.layer_id,
            }
            if close_err ~= nil then
              error(close_err, 0)
            end
            if not explicit_path then
              return { closed = true, layer_id = layer.layer_id }
            end
          end
        end
      end
    end

    local src_block = find_source_chat_block(win, ctx.block_id)
    local cwd = (args and args.cwd and args.cwd ~= "" and args.cwd) or resolve_block_cwd(src_block)
    local cmd = build_mdpeek_command("float", view.session_id, src_block, cwd, explicit_path)

    local opts = {
      shell = "none",
      command = cmd,
    }
    if cwd and cwd ~= "" then
      opts.cwd = cwd
    end

    local created, create_err = rex.session.new_layer{
      window_id = win.window_id,
      bounds = { 0.04, 0.05, 0.92, 0.90 },
      focus = true,
      layout = {
        block = {
          flavor = SHELL_FLAVOR,
          label = FLOAT_LABEL,
          options = opts,
        },
      },
    }
    if create_err ~= nil then
      error(create_err, 0)
    end
    local new_block_id = created.block_ids and created.block_ids[1] or nil
    if new_block_id then
      rex.session.set_block_label{
        block_id = new_block_id,
        label = FLOAT_LABEL,
      }
    end
    return {
      opened = true,
      mode = "float",
      layer_id = created.layer_id,
      block_id = new_block_id,
      cwd = cwd,
      path = explicit_path,
    }
  end,
}

rex.action{
  name = "mdpeek_dock",
  title = "Markdown Peek: Toggle Docked Diagram & Doc Split",
  description = "Open or close a right-hand docked Markdown & Pele diagram preview panel that follows your chat terminal.",
  category = "View",
  keywords = "markdown md peek diagram mermaid pele svg dock split preview",
  args = {
    path = "string?",
    cwd = "string?",
    ratio = "number?",
  },
  run = function(ctx, args)
    local view, err = rex.session.view{}
    if not view then
      error(err or "failed to inspect Rex session", 0)
    end
    local win = resolve_active_window(view)
    if not win then
      error("no active window in current session", 0)
    end

    local explicit_path = args and args.path and args.path ~= "" and args.path or nil

    for _, layer in ipairs(win.layers or {}) do
      if layer.kind == "tiled" then
        for _, blk in ipairs(layer.blocks or {}) do
          if blk.label == DOCK_LABEL then
            local _, close_err = rex.call("block.close", {
              session_id = view.session_id,
              block_id = blk.block_id,
            })
            if close_err ~= nil then
              error(close_err, 0)
            end
            if not explicit_path then
              return { closed = true, block_id = blk.block_id }
            end
          end
        end
      end
    end

    local src_block = find_source_chat_block(win, ctx.block_id)
    local cwd = (args and args.cwd and args.cwd ~= "" and args.cwd) or resolve_block_cwd(src_block)
    local cmd = build_mdpeek_command("dock", view.session_id, src_block, cwd, explicit_path)

    local opts = {
      shell = "none",
      command = cmd,
    }
    if cwd and cwd ~= "" then
      opts.cwd = cwd
    end

    local anchor_id = src_block
    for _, layer in ipairs(win.layers or {}) do
      if layer.kind == "tiled" then
        if layer.layout and layer.layout.split and layer.layout.split.split_id then
          anchor_id = layer.layout.split.split_id
        elseif not anchor_id and layer.blocks and layer.blocks[1] then
          anchor_id = layer.blocks[1].block_id
        end
        break
      end
    end

    local split_ratio = (args and args.ratio) or 0.56
    local created, split_err = rex.session.new_split{
      anchor_id = anchor_id,
      direction = "horizontal",
      side = "after",
      ratio = split_ratio,
      focus = true,
      layout = {
        block = {
          flavor = SHELL_FLAVOR,
          label = DOCK_LABEL,
          options = opts,
        },
      },
    }
    if split_err ~= nil then
      error(split_err, 0)
    end
    local new_block_id = created.block_ids and created.block_ids[1] or nil
    if new_block_id then
      rex.session.set_block_label{
        block_id = new_block_id,
        label = DOCK_LABEL,
      }
    end
    return {
      opened = true,
      mode = "dock",
      block_id = new_block_id,
      cwd = cwd,
      path = explicit_path,
    }
  end,
}

local function register_default_binding(chord, action_name)
  pcall(function()
    local existing = rex.bindings{ key = chord }
    if not existing or #existing == 0 then
      rex.bind(chord, action_name)
    end
  end)
end

if IS_WINDOWS then
  register_default_binding("ctrl+shift+m", "mdpeek_toggle")
  register_default_binding("ctrl+alt+m", "mdpeek_dock")
else
  register_default_binding("shift+cmd+m", "mdpeek_toggle")
  register_default_binding("alt+cmd+m", "mdpeek_dock")
end
