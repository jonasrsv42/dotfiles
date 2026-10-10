-- The bloaty lens: inside a Rust buffer, marks only what earned the file its
-- smell dot (the culprit fns of the dimensions in `smell.why`) and its `•`
-- (duplicate / unused / single-impl-trait findings), as a terse verdict plus
-- evidence at the end of the line:
--   ◂ split? 100 lines · cc 14
--   ◂ duplicate · pollen/sys.rs:27
-- At most MAX_LINES lines per file. Data comes from mbloaty's cached root
-- runs: entering a buffer starts no process (beyond recomputing a stale
-- root, or the file's project root once if no root covers it, and a
-- recompute when the file changed on disk since the cached run).
--
-- Extmarks, not diagnostics (<C-j>/<C-k> jump diagnostics). End-of-line
-- virtual text is laid out lowest priority first, so the lens takes a
-- higher priority than diagnostic virtual text (4096) to stay after
-- rust-analyzer's messages.

local M = {}

local ns = vim.api.nvim_create_namespace("bloaty_lens")
local PRIORITY = 5000
local MAX_LINES = 4
local bloaty -- the mbloaty module

local function file_of(buf)
  if not vim.api.nvim_buf_is_valid(buf) or vim.bo[buf].buftype ~= "" then
    return nil
  end
  local name = vim.api.nvim_buf_get_name(buf)
  -- Absolute paths only: not diffview://, oil://, ...
  return name:sub(1, 1) == "/" and name:sub(-3) == ".rs" and name or nil
end

-- `Node<A, B>::f` -> `Node<…>::f`; `<Node<A> as Build>::f` -> `<Node<…> as
-- Build>::f` (only generic arguments collapse).
local function short(name)
  local s = (name or "?"):gsub("([%w_])%b<>", "%1<…>")
  return s
end

-- Between a verdict and its evidence: a space after `split?` / `panics:`,
-- else a dot (`duplicate · pollen/sys.rs:26`).
local function sep(verdict)
  local last = verdict:sub(-1)
  return (last == "?" or last == ":") and " " or " · "
end

-- Culprits are marked only past these, though `why` (percentiles) names
-- more: in a big workspace a cc 4 or a 45-line fn can be top decile.
local MIN_SPLIT_LINES = 60
local MIN_SIMPLIFY_CC = 8

-- `other` relative to `this`: past their shared directories, at most the
-- last two components (korvo2/sys.rs vs pollen/sys.rs -> pollen/sys.rs).
local function rel(other, this)
  if other == this then
    return "line"
  end
  local a, b = vim.split(other, "/"), vim.split(this, "/")
  local i = 1
  while i < #a and i < #b and a[i] == b[i] do
    i = i + 1
  end
  return table.concat(a, "/", math.max(i, #a - 1))
end

local function where(o, this)
  local r = rel(o.path, this)
  return r == "line" and ("line " .. o.line) or (r .. ":" .. o.line)
end

-- "3 unwrap, 1 expect" from panic sites.
local function kinds(sites)
  local count, order = {}, {}
  for _, s in ipairs(sites or {}) do
    if not count[s.kind] then
      table.insert(order, s.kind)
    end
    count[s.kind] = (count[s.kind] or 0) + 1
  end
  table.sort(order, function(x, y)
    return count[x] > count[y]
  end)
  local parts = {}
  for i, k in ipairs(order) do
    if i > 3 then
      table.insert(parts, "…")
      break
    end
    table.insert(parts, count[k] .. " " .. k)
  end
  return table.concat(parts, ", ")
end

-- The smell culprits, in order: the dimensions named in `why` only (past
-- the floors above unless `all`).
-- Each: { line, verdict, ev, merged (evidence when joining another
-- culprit's line), list (quickfix text), dim }.
local function culprits(g, all)
  local s = g and g.smell
  local why = s and type(s.level) == "string" and s.why
  if not why then
    return {}
  end
  local out = {}
  local size = why:match("size: ([^;]*)")
  local cx = why:match("complexity: ([^;]*)")
  local frag = why:match("fragility: ([^;]*)")
  local f = g.largest_fn
  if size and size:find("largest fn", 1, true) and f and (all or f.lines >= MIN_SPLIT_LINES) then
    table.insert(out, {
      line = f.line, verdict = "split?", ev = f.lines .. " lines", merged = f.lines .. " lines",
      list = string.format("split? %s: %d lines", short(f.name), f.lines), dim = "size",
    })
  elseif size and size:find("impl lines", 1, true) then
    table.insert(out, {
      line = 1, verdict = "big file?", ev = g.impl .. " impl lines", merged = g.impl .. " impl lines",
      list = string.format("big file? %d impl lines", g.impl), dim = "size",
    })
  end
  local c = g.max_complexity
  if cx and cx:find("cyclomatic complexity ", 1, true) and c and (all or c.value >= MIN_SIMPLIFY_CC) then
    table.insert(out, {
      line = c.line, verdict = "simplify?", ev = "cyclomatic complexity " .. c.value, merged = "cyclomatic complexity " .. c.value,
      list = string.format("simplify? %s: cyclomatic complexity %d", short(c.name), c.value), dim = "complexity",
    })
  end
  local n = g.max_nesting
  if cx and cx:find("nesting ", 1, true) and n then
    table.insert(out, {
      line = n.line, verdict = "flatten?", ev = "nesting " .. n.value, merged = "nesting " .. n.value,
      list = string.format("flatten? %s: nesting %d", short(n.name), n.value), dim = "complexity",
    })
  end
  local p = g.most_panics
  if frag and p then
    local ev = kinds(p.sites)
    if (g.panics or 0) > p.value then
      ev = ev .. " · " .. g.panics .. " in file"
    end
    table.insert(out, {
      line = p.line, verdict = "panics:", ev = ev, merged = "panics: " .. kinds(p.sites),
      list = string.format("panics: %s: %s (%d in file)", short(p.name), kinds(p.sites), g.panics or 0),
      dim = "fragility", sites = p.sites,
    })
  end
  return out
end

-- A finding as a note: { line, verdict, ev, list }.
local function finding_note(f, this)
  local note = { line = f.line }
  if f.kind == "duplicate" then
    local o = f.others[1] or {}
    note.verdict = f.identical == false and "near-duplicate" or "duplicate"
    if o.name and o.name ~= f.name then
      note.verdict = note.verdict .. " of " .. short(o.name)
    end
    note.ev = o.path and where(o, this) or nil
    if note.ev and #f.others > 1 then
      note.ev = note.ev .. " +" .. (#f.others - 1)
    end
  elseif f.kind == "unused" then
    note.verdict = "unused"
    -- "no references" is what unused means; keep only what's beyond it.
    local extra = {}
    for _, part in ipairs(vim.split(f.detail or "", "; ", { plain = true })) do
      if part ~= "" and part ~= "no references" then
        table.insert(extra, (part:gsub("^%+", "")))
      end
    end
    note.ev = #extra > 0 and table.concat(extra, " · ") or nil
  elseif f.kind == "single_impl_trait" then
    local o = f.others[1]
    note.verdict = o and "only impl:" or "no impls"
    note.ev = o and short(o.name) or nil
  else
    note.verdict = f.kind
  end
  note.list = (f.kind == "duplicate" and "" or (short(f.name) .. ": "))
    .. note.verdict .. (note.ev and (sep(note.verdict) .. note.ev) or "")
  return note
end

-- The notes to show, grouped by line: { [line] = {notes...} }, at most
-- MAX_LINES lines, culprits first, then findings, then (fragility) the
-- worst fn's panic sites.
local function layout(g, fl, this)
  local lines, count = {}, 0
  local function place(note, culprit)
    local at = lines[note.line]
    if at then
      if culprit and at[1].culprit then
        table.insert(at, { ev = note.merged })
      else
        table.insert(at, { verdict = note.verdict, ev = note.ev })
      end
    elseif count < MAX_LINES then
      count = count + 1
      lines[note.line] = { { verdict = note.verdict, ev = note.ev, culprit = culprit } }
    end
  end
  local cs = culprits(g)
  for _, c in ipairs(cs) do
    place(c, true)
  end
  local sorted = vim.deepcopy(fl or {})
  table.sort(sorted, function(a, b)
    return a.line < b.line
  end)
  for _, f in ipairs(sorted) do
    place(finding_note(f, this), false)
  end
  for _, c in ipairs(cs) do
    for _, s in ipairs(c.sites or {}) do
      if not lines[s.line] then
        place({ line = s.line, verdict = s.kind }, false)
      end
    end
  end
  return lines
end

local function clear(buf)
  if vim.api.nvim_buf_is_valid(buf) then
    vim.api.nvim_buf_clear_namespace(buf, ns, 0, -1)
  end
end

-- Whether `path` was modified after `t` ({sec, nsec}).
local function newer(path, t)
  local st = t and vim.uv.fs_stat(path)
  if not st then
    return false
  end
  return st.mtime.sec > t.sec or (st.mtime.sec == t.sec and st.mtime.nsec > t.nsec)
end

--- Redraws the lens of `buf` from the cache. Leaves modified buffers alone:
--- their marks have moved with the edits, the cached lines haven't.
function M.render(buf)
  local path = file_of(buf)
  if not path or not vim.api.nvim_buf_is_loaded(buf) then
    return
  end
  if not bloaty.enabled() or vim.b[buf].bloaty_lens_off then
    return clear(buf)
  end
  if vim.bo[buf].modified then
    return
  end
  local g, fl, r = bloaty.lookup(path, true)
  if r and (r.busy or newer(path, r.started)) then
    -- Saved or changed on disk since the cache was computed: its lines may
    -- be off. Keep the marks (they moved with any edits here) and
    -- recompute; the refresh renders again.
    if not r.busy and not (r.timer and r.timer:is_active()) then
      bloaty.changed(path)
    end
    return
  end
  clear(buf)
  if not g then
    return
  end
  local level = g.smell and type(g.smell.level) == "string" and g.smell.level
  local hl = level == "hot" and "BloatyLensHot" or level == "warm" and "BloatyLensWarm" or "BloatyLensMark"
  local n = vim.api.nvim_buf_line_count(buf)
  for line, notes in pairs(layout(g, fl, g.path)) do
    if line >= 1 and line <= n then
      local chunks = { { "◂ ", hl } }
      for i, note in ipairs(notes) do
        if i > 1 then
          table.insert(chunks, { " · ", "BloatyLensEvidence" })
        end
        if note.verdict then
          table.insert(chunks, { note.verdict, hl })
        end
        if note.ev then
          table.insert(chunks, { (note.verdict and sep(note.verdict) or "") .. note.ev, "BloatyLensEvidence" })
        end
      end
      pcall(vim.api.nvim_buf_set_extmark, buf, ns, line - 1, 0, {
        virt_text = chunks,
        virt_text_pos = "eol",
        hl_mode = "combine",
        priority = PRIORITY,
        -- Gone when its line is deleted.
        invalidate = true,
      })
    end
  end
end

local function render_visible()
  local seen = {}
  for _, win in ipairs(vim.api.nvim_list_wins()) do
    local buf = vim.api.nvim_win_get_buf(win)
    if not seen[buf] then
      seen[buf] = true
      M.render(buf)
    end
  end
end

--- Toggles the lens in the current buffer.
function M.toggle_buffer()
  local buf = vim.api.nvim_get_current_buf()
  vim.b[buf].bloaty_lens_off = not vim.b[buf].bloaty_lens_off
  M.render(buf)
end

local function say(msg)
  vim.notify(msg, vim.log.levels.INFO, { title = "bloaty" })
end

-- The current buffer's file and cached data, or nil after saying why.
local function current()
  local buf = vim.api.nvim_get_current_buf()
  local path = file_of(buf)
  if not path then
    return say("not a Rust file")
  end
  local g, fl, _, why = bloaty.lookup(path, true)
  if not g then
    return say(({
      computing = "bloaty: computing…",
      unmeasured = "bloaty has no numbers for this file (test-only, not compiled, or new since the last run)",
    })[why] or "not in a cargo project bloaty can measure")
  end
  return buf, path, g, fl or {}
end

--- Fills the quickfix list with the file's culprits, findings
--- (other duplicate copies and trait impls as entries of their own) and,
--- when it has any, its panic sites (from a fresh `bloaty metrics` run).
function M.qflist()
  local buf, path, g, fl = current()
  if not buf then
    return
  end
  local win = vim.api.nvim_get_current_win()
  local items = {}
  local function add(file, line, text)
    table.insert(items, { filename = file, lnum = line, text = text })
  end
  for _, c in ipairs(culprits(g, true)) do
    add(path, c.line, string.format("%s (%s)", c.list, g.smell.level))
  end
  local sorted = vim.deepcopy(fl)
  table.sort(sorted, function(a, b)
    return a.line < b.line
  end)
  for _, f in ipairs(sorted) do
    add(path, f.line, finding_note(f, g.path).list)
    local label = f.kind == "duplicate" and "copy" or "impl"
    for _, o in ipairs(f.others) do
      add(o.path, o.line, string.format("  %s: %s · %s", label, short(o.name), where(o, g.path)))
    end
  end
  local function show()
    if not vim.api.nvim_win_is_valid(win) then
      return
    end
    if #items == 0 then
      return say(vim.fs.basename(path) .. ": no smell culprits or findings")
    end
    -- A new list on the quickfix stack (:colder brings the previous back);
    -- step through it with :cnext / :cprevious.
    vim.fn.setqflist({}, " ", { title = "bloaty: " .. vim.fn.fnamemodify(path, ":~:."), items = items })
    vim.api.nvim_win_call(win, function()
      vim.cmd("copen")
    end)
  end
  if (g.panics or 0) == 0 then
    return show()
  end
  bloaty.run({ "metrics", path, "--json" }, vim.fs.dirname(path), function(m)
    for _, fn in ipairs(m and m.functions or {}) do
      for _, s in ipairs(fn.panic_sites or {}) do
        -- bloaty counts characters; the list wants bytes.
        local col = s.col
        if vim.api.nvim_buf_is_valid(buf) then
          local text = vim.api.nvim_buf_get_lines(buf, s.line - 1, s.line, false)[1]
          local ok, byte = pcall(vim.str_byteindex, text or "", "utf-32", s.col - 1, false)
          col = ok and byte + 1 or col
        end
        table.insert(items, {
          filename = path, lnum = s.line, col = col,
          text = string.format("panic: %s in %s", s.kind, short(fn.name)),
        })
      end
    end
    show()
  end)
end

--- A float for the fn under the cursor: its numbers, panic sites, findings
--- and whether it's the file's smell culprit. Closes on cursor move.
function M.info()
  local buf, path, g, fl = current()
  if not buf then
    return
  end
  local win = vim.api.nvim_get_current_win()
  local row = vim.api.nvim_win_get_cursor(win)[1]
  local modified = vim.bo[buf].modified
  bloaty.run({ "metrics", path, "--json" }, vim.fs.dirname(path), function(m)
    if vim.api.nvim_get_current_win() ~= win or vim.api.nvim_win_get_buf(win) ~= buf then
      return
    end
    local real = vim.uv.fs_realpath(path) or path
    local best
    for _, fn in ipairs(m and m.functions or {}) do
      if fn.path == real and fn.start_line <= row and row <= fn.end_line
        and (not best or fn.end_line - fn.start_line < best.end_line - best.start_line) then
        best = fn
      end
    end
    if not best then
      return say("no measured fn under the cursor (tests aren't measured)")
    end
    local lines = {
      short(best.name),
      string.format("%d lines · cyclomatic complexity %d · nesting %d · %d params · %d clones",
        best.lines, best.complexity, best.nesting, best.params, best.clones),
    }
    if best.panics > 0 then
      local at = {}
      for _, s in ipairs(best.panic_sites or {}) do
        table.insert(at, s.kind .. " :" .. s.line)
      end
      table.insert(lines, string.format("%d panics: %s", best.panics, table.concat(at, ", ")))
    else
      table.insert(lines, "no panics")
    end
    for _, c in ipairs(culprits(g, true)) do
      if c.line == best.line then
        table.insert(lines, string.format("◂ %s %s: why the file is %s (%s)", c.verdict, c.ev, g.smell.level, c.dim))
      end
    end
    for _, f in ipairs(fl) do
      if best.start_line <= f.line and f.line <= best.end_line then
        local note = finding_note(f, g.path)
        table.insert(lines, "◂ " .. note.list)
      end
    end
    if modified then
      table.insert(lines, "(unsaved changes: numbers are from the file on disk)")
    end
    vim.lsp.util.open_floating_preview(lines, "", { border = "rounded", focus_id = "bloaty_info" })
  end)
end

function M.setup(mbloaty)
  bloaty = mbloaty
  for name, link in pairs({
    BloatyLensHot = "BloatySmellHot",
    BloatyLensWarm = "BloatySmellWarm",
    BloatyLensMark = "BloatyMark",
    BloatyLensEvidence = "Comment",
  }) do
    vim.api.nvim_set_hl(0, name, { link = link, default = true })
  end
  vim.keymap.set("n", "<leader>bb", M.toggle_buffer, { desc = "bloaty: toggle the lens in this buffer" })
  vim.keymap.set("n", "<leader>bl", M.qflist, { desc = "bloaty: culprits and findings of this file" })
  vim.keymap.set("n", "<leader>bi", M.info, { desc = "bloaty: the fn under the cursor" })
  -- BufReadPost/FileChangedShellPost: reloaded after a change on disk
  -- (checkout, pull) while shown.
  vim.api.nvim_create_autocmd({ "BufWinEnter", "BufReadPost", "FileChangedShellPost" }, {
    pattern = "*.rs",
    group = vim.api.nvim_create_augroup("mbloaty_lens", { clear = true }),
    callback = function(ev)
      M.render(ev.buf)
    end,
  })
  -- After a root's runs finish (or the toggle): every shown buffer. Hidden
  -- ones catch up when shown.
  bloaty.on_refresh(function(_, r)
    if r == nil then
      -- Toggled: also clear hidden buffers.
      for _, buf in ipairs(vim.api.nvim_list_bufs()) do
        if not bloaty.enabled() then
          clear(buf)
        end
      end
    end
    render_visible()
  end)
end

return M
