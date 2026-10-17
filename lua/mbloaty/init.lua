-- bloaty in Neovim: where the code of a Rust project lives, and where it is
-- bloated, at a glance. This module runs bloaty and caches its answers per
-- root; the views read them:
--   lua/mbloaty/tree.lua  the neo-tree column: a dim line count and bar `▂ ▄ ▆` for
--                         a file's or directory's share of the
--                         implementation lines (tests, docs and comments
--                         excluded) of the directory it's in (blank under
--                         3%, under 20 lines or for an only child; the
--                         count is in `B`), with to its left
--     `•`  on files/directories holding bloat findings (duplicate bodies,
--          unused code, single-impl traits), and
--     `●`  on files that smell: red = hot (worst ~5% of the workspace by
--          size, complexity or panics), yellow = warm (next ~10%), blue =
--          noted (would be hot or warm but for a table, a dispatch's width
--          or an allowed clippy lint: worth a look, not a smell).
--          Directories get a red `●` when a hot file is inside.
--   lua/mbloaty/lens.lua  in a Rust buffer, marks only what earned the file
--                         its dot or `•`, at the end of the culprit fn's
--                         line: `◂ split? 100 lines · cognitive complexity
--                         22`, `◂ duplicate of
--                         f · other/file.rs:27`, ...; and on the first of
--                         tests that differ only in literals or open
--                         alike: `◂ parametrize? 5 tests · 3 literals`,
--                         `◂ fixture? 3 tests share 6 statements`
--   lua/mbloaty/text.lua  the words both use.
-- See tools/bloaty: `bloaty summary` / `findings` / `metrics` --help.
--
-- The numbers come from two background `bloaty` runs per root (~0.1s on a
-- 200k-line workspace), redone once saves and file changes settle;
-- rendering only looks them up. Roots bloaty refuses (not in a cargo
-- project or git repository) show nothing and run nothing again.
--   <leader>bt     toggle the column and the lens
--   B (in tree)    explain the file/directory under the cursor
--   <leader>bb     toggle the lens in this buffer
--   <leader>bl     quickfix list: this file's culprits, excused fns,
--                  findings (each duplicate copy / trait impl /
--                  candidate's test too), panic sites, then duplicate notes
--   <leader>bi     float: the fn under the cursor's numbers and findings
--                  (on a test: the table-driven-test candidates it is in)
--   :BloatyRefresh recompute now (also re-checks refused roots)

local text = require("mbloaty.text")

local M = {}

local enabled = true
-- Root directory (as the tree has it) -> {
--   real      its realpath (bloaty's paths are real)
--   ready     the last runs' data below is in place
--   loc       {path -> impl lines}, files and directories
--   kids      {directory -> number of its children with impl lines}
--   marks     {path -> number of findings}, files and directories
--   tables    {path -> number of table-driven-test candidates
--             (`parametrize`)}, files and directories (listed, never
--             marked: test code)
--   setups    {path -> number of fixture candidates (`fixture`)}, the same
--   smell     {path -> "hot" | "warm" | "noted"} (directories: "hot" only)
--   files     {path -> summary group}
--   findings  {path -> findings located in it, see `file_findings`}
--   workspace bloaty's root for it (smell ranks across all of it)
--   started   when the last runs started (realtime {sec, nsec}); files
--             changed after it may be off (the lens checks)
--   busy      runs in flight
--   outdated  the binary's stamp when its output was too old: nothing is
--             cached until the binary changes
--   skip      bloaty refused the root
--   stale     changed while no view watched it; recomputed when one does
--   gen, jobs, timer  run bookkeeping
-- }
local roots = {}
-- Findings fetched, by their JSON names (`--kind` takes them too): listed
-- per file (lens, quickfix, `B`)...
local KINDS = { "duplicate", "unused", "single_impl_trait", "parametrize", "fixture" }
-- ...and those that mark a file as bloated (tree `•`). Long fns and
-- test-heavy files are common and judgement calls, so they aren't fetched;
-- table-driven-test and fixture candidates are test code, so they're
-- listed, never marked.
local MARK_KINDS = { duplicate = true, unused = true, single_impl_trait = true }
local DEBOUNCE_MS = 1500
-- The `--json` format this code reads (bloaty's `schema` field). Older or
-- unversioned output is refused, not misread.
local SCHEMA = 14
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

-- Calls `f(dir)` for `path` and every directory above it, up to `top`.
local function up(path, top, f)
  while path and #path >= #top do
    f(path)
    if path == top then
      return
    end
    path = vim.fs.dirname(path)
  end
end

-- Runs `bloaty args` and calls `cb(decoded_json or nil, exit_code)` on the
-- main loop; (nil, "outdated") after one warning when the binary's output
-- is older than `SCHEMA`. Exit 1 ("no .rs files here": a docs or Android
-- dir inside a workspace) and 2 (refused: not a project) are expected and
-- quiet.
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
      data = decoded and data or nil
      if type(data) == "table" and (tonumber(data.schema) or 0) < SCHEMA then
        notify_once(text.OUTDATED)
        return cb(nil, "outdated")
      end
      -- Warn again if a later binary is outdated once more.
      notified[text.OUTDATED] = nil
      cb(data, 0)
    end)
  end)
  -- e.g. the root directory was deleted (cwd ENOENT).
  return ok and job or nil
end

-- Where a finding is, per file: { kind, name, line, detail, note, test,
-- dup, tests, literals, prefix, others = { {name, file, path, line} } }. A duplicate is at each
-- copy, its others the other copies, `dup` the group (facts: `identical`,
-- `crates`, `depends`, `differs`, `hint`, `rule`, copies with their
-- signature `class`), `note` true for a note-tier one and `test` for a
-- test copy (both listed, never marked); a single-impl trait's others are
-- its impls; a table-driven-test candidate (`parametrize`) is at its first
-- test, `tests` all of them ({name, path, line, end_line, lines,
-- identical}), `literals` {differ, of}; a fixture candidate (`fixture`)
-- too, with `tests` (no `identical`) and `prefix` {statements, lines}.
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
        note = finding.tier == "note", test = c.test, dup = finding, others = others,
      })
    end
  else
    at(finding.path, {
      kind = finding.kind, name = finding.name, line = finding.line,
      detail = finding.detail, others = finding.impls or {}, tests = finding.tests, literals = finding.literals,
      prefix = finding.prefix,
    })
  end
end

-- Identifies the installed binary: its path and mtime.
local function binary_stamp()
  local exe = vim.fn.exepath("bloaty")
  local st = exe ~= "" and vim.uv.fs_stat(exe)
  return st and string.format("%s:%d.%09d", exe, st.mtime.sec, st.mtime.nsec) or ""
end

-- Starts the root's two runs (`summary --by file`, `findings`), replacing
-- any in flight; listeners hear when both land.
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
  r.started, r.busy = vim.uv.clock_gettime("realtime"), true
  local gen, pending = r.gen, 2
  local loc, kids, marks, tables, setups, smell, files, by_file = {}, {}, {}, {}, {}, {}, {}, {}
  local refused, outdated = false, false
  local function done(code)
    refused = refused or code == 2
    outdated = outdated or code == "outdated"
    pending = pending - 1
    if pending > 0 or gen ~= r.gen then
      return
    end
    r.busy, r.outdated = false, nil
    if refused then
      r.skip = true
    elseif outdated then
      -- Nothing cached: recomputed once the binary changes (see `data`).
      r.ready, r.outdated = false, binary_stamp()
    else
      r.loc, r.kids, r.marks, r.tables, r.setups, r.smell, r.files, r.findings, r.ready =
        loc, kids, marks, tables, setups, smell, files, by_file, true
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
        up(g.path, r.real, function(p)
          if not loc[p] and p ~= r.real then
            local d = vim.fs.dirname(p)
            kids[d] = (kids[d] or 0) + 1
          end
          loc[p] = (loc[p] or 0) + g.impl
        end)
        local level = text.level(g)
        if level == "hot" then
          -- Hot shows up the tree; warm and noted only on the file itself
          -- (they would colour most of the tree).
          up(g.path, r.real, function(p)
            smell[p] = "hot"
          end)
        elseif level and smell[g.path] ~= "hot" then
          smell[g.path] = level
        end
      end
    end
    done(code)
  end)
  local args = { "findings", dir, "--json", "--top", "0" }
  for _, kind in ipairs(KINDS) do
    vim.list_extend(args, { "--kind", kind })
  end
  local findings = run(args, dir, function(f, code)
    for _, finding in ipairs(f and f.findings or {}) do
      if not MARK_KINDS[finding.kind] then
        file_findings(by_file, finding)
        local counts = ({ parametrize = tables, fixture = setups })[finding.kind]
        if counts then
          up(finding.path, r.real, function(d)
            counts[d] = (counts[d] or 0) + 1
          end)
        end
      -- Like the counts, marks are about implementation code: a marking
      -- kind's test findings aren't kept; its notes and test copies are
      -- listed, never marked.
      elseif finding.category ~= "test" then
        -- Every impl copy of a duplicate is a place to look.
        for _, p in ipairs(finding.tier ~= "note" and (finding.copies or { finding }) or {}) do
          if not p.test then
            up(p.path, r.real, function(d)
              marks[d] = (marks[d] or 0) + 1
            end)
          end
        end
        file_findings(by_file, finding)
      end
    end
    done(code)
  end)
  -- Callbacks run later (scheduled), so this settles `pending` first. A job
  -- that didn't start (no bloaty, root gone) never calls back.
  r.jobs = vim.tbl_filter(function(j) return j ~= nil end, { summary, findings })
  pending = #r.jobs
  if pending == 0 then
    r.skip, r.busy = true, false
  end
end

-- The cached data for a root, computed on first use, again when it went
-- stale while hidden, and when its runs found the binary outdated and it
-- has been reinstalled since.
local function data(dir)
  local r = roots[dir]
  if not r then
    r = { real = vim.uv.fs_realpath(dir) or dir }
    roots[dir] = r
    refresh(dir)
  elseif r.stale or (r.outdated and not r.busy and r.outdated ~= binary_stamp()) then
    refresh(dir)
  end
  return r
end

--- Recompute the roots holding `path` once changes settle: now if a tree
--- shows them or a window a Rust file in them, else when one next does.
function M.changed(path)
  if path == "" or path:find("/target/", 1, true) or path:find("/%.git/") then
    return
  end
  local shown = shown_roots()
  local visible = {}
  for _, win in ipairs(vim.api.nvim_list_wins()) do
    table.insert(visible, vim.api.nvim_buf_get_name(vim.api.nvim_win_get_buf(win)))
  end
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

--- The cached data of the root at `dir` (see `roots`), starting its runs
--- if needed.
M.root = data

--- The data of a root already known, or nil.
function M.known(dir)
  return roots[dir]
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

--- `bloaty metrics FILE --json` for the fns of `path`; `cb(report or nil)`.
function M.metrics(path, cb)
  return run({ "metrics", path, "--json" }, vim.fs.dirname(path), cb)
end

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
--- has no numbers for it), "outdated" (the binary is) or "refused". Starts
--- no run except to recompute a stale root (or an outdated one after a
--- reinstall), or, when `start` is set and no root covers the file, one at
--- the cwd (where a tree would open) if that holds the file, else at its
--- project (git or cargo root).
function M.lookup(path, start)
  local real = vim.uv.fs_realpath(path) or path
  local found, covered, pending, outdated
  for dir, r in pairs(roots) do
    if (inside(real, r.real) or inside(path, dir)) and not r.skip then
      covered, pending = true, pending or not r.ready
      data(dir)
      outdated = outdated or r.outdated ~= nil
      if r.ready and r.files[real] then
        found = r
      end
    end
  end
  if found then
    return found.files[real], found.findings[real], found
  end
  if covered then
    return nil, nil, nil, outdated and "outdated" or pending and "computing" or "unmeasured"
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
    -- Size isn't a finding: dim as neo-tree's own dim text, never a
    -- diagnostic colour (those are for the marks). Linked unconditionally:
    -- neo-tree defines NeoTreeDimText only when the tree first opens, which
    -- is before the bar is ever drawn.
    BloatySize = "NeoTreeDimText",
    BloatyMark = "DiagnosticHint",
    BloatySmellHot = "DiagnosticError",
    BloatySmellWarm = "DiagnosticWarn",
    BloatySmellNoted = "DiagnosticInfo",
  }) do
    vim.api.nvim_set_hl(0, name, { link = link, default = true })
  end

  vim.keymap.set("n", "<leader>bt", M.toggle, { desc = "bloaty: toggle neo-tree column and lens" })
  vim.api.nvim_create_user_command("BloatyRefresh", M.refresh_all, {})

  vim.api.nvim_create_autocmd("BufWritePost", {
    pattern = "*.rs",
    group = vim.api.nvim_create_augroup("mbloaty", { clear = true }),
    callback = function(ev)
      M.changed(vim.api.nvim_buf_get_name(ev.buf))
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
        M.changed(args and args.afile or "")
      end,
    })
  end
  require("mbloaty.lens").setup(M)
end

return M
