-- bloaty in neo-tree: where the code of a Rust project lives, at a glance.
--
-- A dim column of implementation line counts beside every file and
-- directory (tests, docs and comments excluded), coloured warmer where code
-- piles up, with
--   `•`  on files/directories holding bloat findings (duplicate bodies,
--        unused code, single-impl traits), and
--   `●`  on files that smell: red = hot (worst ~5% of the workspace by
--        size, complexity or panics), yellow = warm (next ~10%).
--        Directories get a red `●` when a hot file is inside.
-- `B` in the tree explains a file's numbers. See tools/bloaty: `bloaty
-- summary` / `bloaty findings` / `bloaty metrics` explain them too.
--
-- In a Rust buffer the lens (lua/mbloaty_lens.lua) marks only what earned
-- the file its dot or `•`: `◂ split? 100 lines · cc 14` at the end of the
-- culprit fn's line, `◂ duplicate of f · other/file.rs:27`, ... It reuses
-- the tree's cached runs (opening a Rust file starts them for its project if
-- no tree has).
--
-- The numbers come from two background `bloaty` runs per tree root (~0.1s
-- on a 200k-line workspace), redone once saves and file changes settle;
-- rendering only looks them up. Roots bloaty refuses (not in a cargo
-- project or git repository) show nothing and run nothing again.
--   <leader>bt     toggle the column and the lens
--   B (in tree)    explain the file/directory under the cursor
--   <leader>bb     toggle the lens in this buffer
--   <leader>bl     quickfix list: this file's culprits, findings (each
--                  duplicate copy / trait impl too) and panic sites
--   <leader>bi     float: the fn under the cursor's numbers and findings
--   :BloatyRefresh recompute now (also re-checks refused roots)

local M = {}

local enabled = true
-- Tree root -> { real = realpath, ready, loc = {path -> impl lines},
-- marks = {path -> findings}, smell = {path -> "hot"|"warm"},
-- files = {path -> summary group}, findings = {path -> list of findings
-- located in it, see `file_findings`}, total, gen, jobs, timer }. `skip`: bloaty
-- refused the root. `stale`: changed while no tree showed it; recomputed
-- when one does.
local roots = {}
-- Findings that mark a file as bloated. Long fns and test-heavy files are
-- common and judgement calls, so they don't.
local MARK_KINDS = { "duplicate", "unused", "single-impl-trait" }
local DEBOUNCE_MS = 1500
-- Called with (dir, root data) when a root's runs finish (the lens).
local listeners = {}

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
      -- JSON nulls as nil (not the truthy vim.NIL).
      local decoded, data =
        pcall(vim.json.decode, res.stdout, { luanil = { object = true, array = true } })
      cb(decoded and data or nil, 0)
    end)
  end)
  -- e.g. the root directory was deleted (cwd ENOENT).
  return ok and job or nil
end

-- Where a finding is, per file: { kind, name, line, detail, others = {
-- {name, file, path, line} } }. A duplicate is at each copy, its others
-- the other copies; a single-impl trait's others are its impls.
local function file_findings(by_file, finding)
  local function at(path, entry)
    by_file[path] = by_file[path] or {}
    table.insert(by_file[path], entry)
  end
  if finding.copies then
    for i, c in ipairs(finding.copies) do
      local others = {}
      for j, o in ipairs(finding.copies) do
        if j ~= i then
          table.insert(others, o)
        end
      end
      at(c.path, {
        kind = finding.kind, name = c.name, line = c.line, detail = finding.detail,
        identical = finding.identical, others = others,
      })
    end
  else
    at(finding.path, {
      kind = finding.kind, name = finding.name, line = finding.line, detail = finding.detail,
      others = finding.impls or {},
    })
  end
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
  -- Files changed after this aren't reflected (the lens checks mtimes);
  -- `busy` until the runs land.
  r.started, r.busy = vim.uv.clock_gettime("realtime"), true
  local gen, pending = r.gen, 2
  local loc, marks, smell, files, total, refused = {}, {}, {}, {}, 0, false
  local by_file = {}
  local function done(code)
    refused = refused or code == 2
    pending = pending - 1
    if pending > 0 or gen ~= r.gen then
      return
    end
    r.busy = false
    if refused then
      r.skip = true
    else
      r.loc, r.marks, r.smell, r.files, r.total, r.ready = loc, marks, smell, files, total, true
      r.findings = by_file
    end
    redraw()
    for _, cb in ipairs(listeners) do
      cb(dir, r)
    end
  end
  local summary = run({ "summary", dir, "--by", "file", "--json" }, dir, function(s, code)
      -- Smell is ranked across the whole workspace: changes anywhere in it
      -- can shift this root's dots (see `changed`).
      r.workspace = s and s.root
      for _, g in ipairs(s and s.groups or {}) do
        if g.path then
          files[g.path] = g
        end
        if g.path and g.impl > 0 then
          add_up(loc, g.path, r.real, g.impl)
          total = total + g.impl
          local level = g.smell and g.smell.level
          if level == "hot" then
            -- Hot shows up the tree; warm only on the file itself (warm
            -- directories would colour most of the tree).
            local p = g.path
            while p and #p >= #r.real do
              smell[p] = "hot"
              if p == r.real then
                break
              end
              p = vim.fs.dirname(p)
            end
          elseif level == "warm" and smell[g.path] ~= "hot" then
            smell[g.path] = "warm"
          end
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
            file_findings(by_file, finding)
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
    r.skip, r.busy = true, false
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
  local visible = {}
  for _, win in ipairs(vim.api.nvim_list_wins()) do
    table.insert(visible, vim.api.nvim_buf_get_name(vim.api.nvim_win_get_buf(win)))
  end
  -- A root is watched while a tree shows it or a window shows a file in it
  -- (the lens).
  local function watched(dir, r)
    if shown[dir] then
      return true
    end
    for _, name in ipairs(visible) do
      if name:sub(-3) == ".rs" and (inside(name, dir) or inside(name, r.real)) then
        return true
      end
    end
    return false
  end
  for dir, r in pairs(roots) do
    local within = inside(path, dir) or inside(path, r.real)
      or (r.workspace and inside(path, r.workspace))
    if not r.skip and within then
      if watched(dir, r) then
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
function M.component(_, node, state, width)
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
  -- When the name leaves no room, neo-tree would cut this column from the
  -- left (229 -> "29"): drop it whole instead. 9 columns + 1 padding.
  if width and vim.api.nvim_strwidth(node.name or "") + 10 > width then
    return {}
  end
  local level = r.smell[key]
  -- The root row would always be hot: leave it plain.
  if node:get_depth() == 1 then
    level = nil
  end
  return {
    { text = r.marks[key] and "•" or " ", highlight = "BloatyMark" },
    {
      text = level and "●" or " ",
      highlight = level == "hot" and "BloatySmellHot" or "BloatySmellWarm",
    },
    { text = string.format("%5s ", human(n)), highlight = heat(node, n, r, key) },
  }
end

--- Explains the node under the cursor in a neo-tree window (mapped to `B`).
function M.explain(state)
  local node = state.tree:get_node()
  local r = state.path and roots[state.path]
  if not node or not node.path then
    return
  end
  local function say(msg)
    vim.notify(msg, vim.log.levels.INFO, { title = "bloaty" })
  end
  if not r or r.skip then
    return say("not a Rust project (bloaty only measures cargo crates)")
  elseif not r.ready then
    return say("still computing…")
  end
  local key = r.real .. node.path:sub(#state.path + 1)
  local g = r.files[key]
  if node.type ~= "directory" and not g then
    return say(node.name .. ": not Rust code bloaty measures")
  end
  local lines = { string.format("%s: %s impl lines", node.name, r.loc[key] or 0) }
  if g then
    table.insert(lines, string.format("tests %d, docs %d, comments %d", g.test, g.doc, g.comment))
    if g.largest_fn then
      table.insert(lines, string.format("largest fn: %s (%d lines)", g.largest_fn.name, g.largest_fn.lines))
    end
    if g.max_complexity then
      table.insert(lines, string.format("max cyclomatic complexity: %d (%s)", g.max_complexity.value, g.max_complexity.name))
    end
    if (g.panics or 0) > 0 then
      table.insert(lines, string.format("panic sites: %d", g.panics))
    end
    local s = g.smell
    if s and type(s.level) == "string" then
      table.insert(lines, string.format("%s: %s", s.level, s.why or ""))
    end
  elseif r.smell[key] == "hot" then
    table.insert(lines, "contains hot files")
  end
  if r.marks[key] then
    table.insert(lines, string.format("%d bloat findings (bloaty findings %s)", r.marks[key], node.path))
  end
  say(table.concat(lines, "\n"))
end

--- Toggles the neo-tree column and the lens.
function M.toggle()
  enabled = not enabled
  redraw()
  for _, cb in ipairs(listeners) do
    cb(nil, nil)
  end
end

--- `bloaty args` in `cwd`; `cb(decoded json or nil, exit code)`.
M.run = run

function M.enabled()
  return enabled
end

--- Calls `cb(dir, root)` whenever a root's runs finish, and `cb(nil, nil)`
--- when the column/lens is toggled.
function M.on_refresh(cb)
  table.insert(listeners, cb)
end

--- The cached data for the Rust file `path`: (summary group or nil, list of
--- findings or nil, root) from a ready root holding it. Without one: (nil,
--- nil, nil, why) with why "computing", "unmeasured" (a root holds it but
--- has no numbers for it) or "refused". Starts no run except
--- to recompute a stale root, or, when `start` is set and no root covers
--- the file, one at the cwd (where a tree would open) if that holds the
--- file, else at its project (git or cargo root).
function M.lookup(path, start)
  local real = vim.uv.fs_realpath(path) or path
  local found, covered, pending
  for dir, r in pairs(roots) do
    if inside(real, r.real) or inside(path, dir) then
      if not r.skip then
        covered, pending = true, pending or not r.ready
        if r.stale then
          refresh(dir)
        end
        if r.ready and r.files[real] then
          found = r
        end
      end
    end
  end
  if found then
    return found.files[real], found.findings[real], found
  end
  if covered then
    return nil, nil, nil, pending and "computing" or "unmeasured"
  end
  if start then
    -- The cwd when it lies inside the file's project (that's where a tree
    -- would open, so they share the runs), else the project itself.
    local project = vim.fs.root(real, ".git") or vim.fs.root(real, "Cargo.toml")
    local cwd = vim.fn.getcwd()
    local real_cwd = vim.uv.fs_realpath(cwd) or cwd
    local dir = project
      and ((inside(real_cwd, project) and inside(real, real_cwd)) and cwd or project)
    if dir and not roots[dir] then
      data(dir)
      return nil, nil, nil, roots[dir].skip and "refused" or "computing"
    end
  end
  return nil, nil, nil, "refused"
end

--- Recompute the roots holding `path` once changes settle.
M.changed = function(path)
  changed(path)
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
    BloatySmellHot = "DiagnosticError",
    BloatySmellWarm = "DiagnosticWarn",
  }) do
    vim.api.nvim_set_hl(0, name, { link = link, default = true })
  end

  vim.keymap.set("n", "<leader>bt", M.toggle, { desc = "bloaty: toggle neo-tree column and lens" })
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
  require("mbloaty_lens").setup(M)
end

return M
