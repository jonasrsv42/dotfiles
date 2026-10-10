-- bloaty in neo-tree: where the code of a Rust project lives, at a glance.
--
-- A dim column of implementation line counts beside every file and
-- directory (tests, docs and comments excluded), coloured warmer where code
-- piles up, with a `•` on files/directories holding bloat findings
-- (duplicate bodies, unused code, single-impl traits). See
-- tools/bloaty: `bloaty summary` / `bloaty findings` explain the numbers.
--
-- The numbers come from two background `bloaty` runs per tree root (~0.1s
-- on a 200k-line workspace), redone once saves and file changes settle;
-- rendering only looks them up. Roots bloaty refuses (not in a cargo
-- project or git repository) show nothing and run nothing again.
--   <leader>bt     toggle the column
--   :BloatyRefresh recompute now (also re-checks refused roots)

local M = {}

local enabled = true
-- Tree root -> { real = realpath, ready, loc = {path -> impl lines},
-- marks = {path -> findings}, total, gen, jobs, timer }. `skip`: bloaty
-- refused the root. `stale`: changed while no tree showed it; recomputed
-- when one does.
local roots = {}
-- Findings that mark a file as bloated. Long fns and test-heavy files are
-- common and judgement calls, so they don't.
local MARK_KINDS = { "duplicate", "unused", "single-impl-trait" }
local DEBOUNCE_MS = 1500

local function redraw()
  local ok, manager = pcall(require, "neo-tree.sources.manager")
  if ok then
    manager.redraw("filesystem")
  end
end

local notified = {}
local function notify_once(msg)
  if not notified[msg] then
    notified[msg] = true
    vim.notify(msg, vim.log.levels.WARN, { title = "bloaty" })
  end
end

-- `path` is `dir` or inside it (not merely sharing a prefix: /a/bc vs /a/b).
local function inside(path, dir)
  return path == dir or vim.startswith(path, dir .. "/")
end

-- Roots of the filesystem trees open in some window right now.
local function shown_roots()
  local shown = {}
  local ok, manager = pcall(require, "neo-tree.sources.manager")
  local ok_r, renderer = pcall(require, "neo-tree.ui.renderer")
  if ok and ok_r then
    for _, state in ipairs(manager._get_all_states()) do
      if state.name == "filesystem" and state.path and renderer.window_exists(state) then
        shown[state.path] = true
      end
    end
  end
  return shown
end

-- Adds `n` to `path` and every directory above it, up to `top`.
local function add_up(map, path, top, n)
  while path and #path >= #top do
    map[path] = (map[path] or 0) + n
    if path == top then
      return
    end
    path = vim.fs.dirname(path)
  end
end

-- Runs `bloaty args` and calls `cb(decoded_json or nil, exit_code)` on the
-- main loop. Exit 1 ("no .rs files here": a docs or Android dir inside a
-- workspace) and 2 (refused: not a project) are expected and quiet.
local function run(args, cwd, cb)
  if vim.fn.executable("bloaty") == 0 then
    notify_once("bloaty not found: cargo install --locked --path ~/dotfiles/tools/bloaty")
    return nil
  end
  local cmd = vim.list_extend({ "bloaty" }, args)
  local ok, job = pcall(vim.system, cmd, { cwd = cwd, text = true }, function(res)
    vim.schedule(function()
      if res.code ~= 0 then
        if res.code > 2 and res.signal == 0 then
          notify_once(vim.trim(res.stderr or ""))
        end
        return cb(nil, res.code)
      end
      local decoded, data = pcall(vim.json.decode, res.stdout)
      cb(decoded and data or nil, 0)
    end)
  end)
  -- e.g. the root directory was deleted (cwd ENOENT).
  return ok and job or nil
end

local function refresh(dir)
  local r = roots[dir]
  if not r or r.skip then
    return
  end
  r.stale = false
  for _, job in ipairs(r.jobs or {}) do
    pcall(job.kill, job, 15)
  end
  r.gen = (r.gen or 0) + 1
  local gen, pending = r.gen, 2
  local loc, marks, total, refused = {}, {}, 0, false
  local function done(code)
    refused = refused or code == 2
    pending = pending - 1
    if pending > 0 or gen ~= r.gen then
      return
    end
    if refused then
      r.skip = true
    else
      r.loc, r.marks, r.total, r.ready = loc, marks, total, true
    end
    redraw()
  end
  local summary = run({ "summary", dir, "--by", "file", "--json" }, dir, function(s, code)
      for _, g in ipairs(s and s.groups or {}) do
        if g.path and g.impl > 0 then
          add_up(loc, g.path, r.real, g.impl)
          total = total + g.impl
        end
      end
      done(code)
    end)
  local findings = run(
      vim.list_extend({ "findings", dir, "--json", "--top", "0" }, {
        "--kind", MARK_KINDS[1], "--kind", MARK_KINDS[2], "--kind", MARK_KINDS[3],
      }),
      dir,
      function(f, code)
        for _, finding in ipairs(f and f.findings or {}) do
          -- Like the counts, marks are about implementation code.
          if finding.category ~= "test" then
            -- Every copy of a duplicate is a place to look.
            for _, p in ipairs(finding.copies or { finding }) do
              add_up(marks, p.path, r.real, 1)
            end
          end
        end
        done(code)
      end
    )
  -- Callbacks run later (scheduled), so this settles `pending` first. A job
  -- that didn't start (no bloaty, root gone) never calls back.
  r.jobs = vim.tbl_filter(function(j) return j ~= nil end, { summary, findings })
  pending = #r.jobs
  if pending == 0 then
    r.skip = true
  end
end

-- The cached data for a tree root, computed on first use and again when it
-- went stale while hidden.
local function data(dir)
  local r = roots[dir]
  if not r then
    r = { real = vim.uv.fs_realpath(dir) or dir }
    roots[dir] = r
    refresh(dir)
  elseif r.stale then
    refresh(dir)
  end
  return r
end

-- Recompute the roots holding `path` once changes settle: now if a tree
-- shows them, else when one next does.
local function changed(path)
  if path == "" or path:find("/target/", 1, true) or path:find("/%.git/") then
    return
  end
  local shown = shown_roots()
  for dir, r in pairs(roots) do
    if not r.skip and (inside(path, dir) or inside(path, r.real)) then
      if shown[dir] then
        r.timer = r.timer or vim.uv.new_timer()
        r.timer:stop()
        r.timer:start(DEBOUNCE_MS, 0, vim.schedule_wrap(function()
          refresh(dir)
        end))
      else
        r.stale = true
      end
    end
  end
end

local function human(n)
  if n < 1000 then
    return tostring(n)
  elseif n < 9950 then
    return string.format("%.1fk", n / 1000)
  end
  return string.format("%dk", math.floor(n / 1000 + 0.5))
end

-- Files: absolute size. Directories: their share of the directory they're
-- in, so the hot spot shows at whatever level you look.
local function heat(node, n, r, key)
  if node.type == "directory" then
    local parent = r.loc[vim.fs.dirname(key)]
    local share = (node:get_depth() > 1 and parent and parent > 0) and n / parent or 0
    if share >= 0.4 then
      return "BloatyHot"
    elseif share >= 0.2 then
      return "BloatyWarm"
    end
    return "BloatyCool"
  end
  if n >= 500 then
    return "BloatyHot"
  elseif n >= 200 then
    return "BloatyWarm"
  end
  return "BloatyCool"
end

--- neo-tree component (`filesystem.components.bloaty`).
function M.component(_, node, state)
  if not enabled or not state.path or not node.path then
    return {}
  end
  local r = data(state.path)
  if r.skip or not r.ready then
    return {}
  end
  local key = r.real .. node.path:sub(#state.path + 1)
  local n = r.loc[key]
  if not n then
    return {}
  end
  return {
    { text = r.marks[key] and "•" or " ", highlight = "BloatyMark" },
    { text = string.format("%5s ", human(n)), highlight = heat(node, n, r, key) },
  }
end

function M.toggle()
  enabled = not enabled
  redraw()
end

--- Recompute every root, re-checking ones bloaty refused before (after a
--- `cargo init`, say).
function M.refresh_all()
  for dir, r in pairs(roots) do
    r.skip = false
    refresh(dir)
  end
end

function M.setup()
  for name, link in pairs({
    BloatyCool = "Comment",
    BloatyWarm = "Normal",
    BloatyHot = "DiagnosticWarn",
    BloatyMark = "DiagnosticHint",
  }) do
    vim.api.nvim_set_hl(0, name, { link = link, default = true })
  end

  vim.keymap.set("n", "<leader>bt", M.toggle, { desc = "bloaty: toggle neo-tree column" })
  vim.api.nvim_create_user_command("BloatyRefresh", M.refresh_all, {})

  vim.api.nvim_create_autocmd("BufWritePost", {
    pattern = "*.rs",
    group = vim.api.nvim_create_augroup("mbloaty", { clear = true }),
    callback = function(ev)
      changed(vim.api.nvim_buf_get_name(ev.buf))
    end,
  })
  -- Changes from outside (branch switch, renames, shell): neo-tree's file
  -- watcher reports the directory.
  local ok, events = pcall(require, "neo-tree.events")
  if ok then
    events.subscribe({
      id = "mbloaty_fs_event",
      event = events.FS_EVENT,
      handler = function(args)
        changed(args and args.afile or "")
      end,
    })
  end
end

return M
