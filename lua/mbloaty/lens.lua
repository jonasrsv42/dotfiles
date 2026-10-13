-- The bloaty lens: inside a Rust buffer, marks only what earned the file its
-- smell dot (the culprits among the signals in `smell.dims`) and its `•`
-- (duplicate / unused / single-impl-trait findings), as a terse verdict plus
-- evidence at the end of the line:
--   ◂ split? 100 lines · cognitive complexity 22
--   ◂ simplify? cognitive complexity 17 · arm Op::Rebalance 16   (at a dispatch's arm)
--   ◂ duplicate · pollen/sys.rs:27
-- and, on the first of tests differing only in literals (no `•`: tests),
--   ◂ parametrize? 5 tests · 3 literals
-- and, in blue, what was excused from the score (the whole story of a noted
-- file's blue dot): `◂ table · 24 entries`, `◂ dispatch · 9 arms ·
-- shared prefix in 4 arms (1 statement)`, `◂ allowed · too_many_lines`.
-- At most MAX_LINES lines per file. Data comes from mbloaty's cached root
-- runs: entering a buffer starts no process (beyond recomputing a stale
-- root, or the file's project root once if no root covers it, and a
-- recompute when the file changed on disk since the cached run).
--
-- Extmarks, not diagnostics (<C-j>/<C-k> jump diagnostics). End-of-line
-- virtual text is laid out lowest priority first, so the lens takes a
-- higher priority than diagnostic virtual text (4096) to stay after
-- rust-analyzer's messages.

local text = require("mbloaty.text")

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

-- The notes to show, grouped by line: { [line] = {notes...} }, at most
-- MAX_LINES lines, culprits first, then excused (noted) fns, then
-- findings, then (fragility) the worst fn's panic sites.
local function layout(g, fl, this)
  local lines, count = {}, 0
  local function place(note, culprit)
    local at = lines[note.line]
    local hl = note.noted and "BloatyLensNoted" or nil
    if at then
      if culprit and at[1].culprit then
        table.insert(at, { ev = note.merged })
      else
        table.insert(at, { verdict = note.verdict, ev = note.ev, hl = hl })
      end
    elseif count < MAX_LINES then
      count = count + 1
      lines[note.line] = { { verdict = note.verdict, ev = note.ev, culprit = culprit, hl = hl } }
    end
  end
  local cs = text.culprits(g)
  for _, c in ipairs(cs) do
    place(c, true)
  end
  for _, n in ipairs(text.notes(g)) do
    place(n, false)
  end
  for _, f in ipairs(text.by_line(fl)) do
    if not (f.note or f.test) then
      place(text.finding_note(f, this), false)
    end
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
  local hl = text.LENS_HL[text.level(g)] or "BloatyLensMark"
  local n = vim.api.nvim_buf_line_count(buf)
  for line, at_line in pairs(layout(g, fl, g.path)) do
    if line >= 1 and line <= n then
      local chunks = { { "◂ ", at_line[1].hl or hl } }
      for i, note in ipairs(at_line) do
        if i > 1 then
          table.insert(chunks, { " · ", "BloatyLensEvidence" })
        end
        if note.verdict then
          table.insert(chunks, { note.verdict, note.hl or hl })
        end
        if note.ev then
          table.insert(chunks, { (note.verdict and text.sep(note.verdict) or "") .. note.ev, "BloatyLensEvidence" })
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

-- The current buffer's file and cached data, or nil after saying why.
local function current()
  local buf = vim.api.nvim_get_current_buf()
  local path = file_of(buf)
  if not path then
    return text.say("not a Rust file")
  end
  local g, fl, _, why = bloaty.lookup(path, true)
  if not g then
    return text.say(({
      computing = "bloaty: computing…",
      outdated = text.OUTDATED,
      unmeasured = "bloaty has no numbers for this file (test-only, not compiled, or new since the last run)",
    })[why] or "not in a cargo project bloaty can measure")
  end
  return buf, path, g, fl or {}
end

--- Fills the quickfix list with the file's culprits, blue notes (each
--- copy of a dispatch's shared prefix too), findings (other duplicate copies,
--- trait impls and a table-driven-test candidate's tests as entries of
--- their own), duplicate notes and, when it has any, its panic sites (from
--- a fresh `bloaty metrics` run).
function M.qflist()
  local buf, path, g, fl = current()
  if not buf then
    return
  end
  local win = vim.api.nvim_get_current_win()
  local items = {}
  local function add(file, line, entry)
    table.insert(items, { filename = file, lnum = line, text = entry })
  end
  -- The file's level is in the title; culprits on one line merge, like the
  -- lens (`split? f: 68 lines · cognitive complexity 12`); blue notes say so.
  local last
  for _, c in ipairs(text.culprits(g, true)) do
    if last and c.merged and last.lnum == c.line and last.filename == path then
      last.text = last.text .. " · " .. c.merged
    else
      add(path, c.line, c.list)
      last = items[#items]
    end
  end
  for _, n in ipairs(text.notes(g)) do
    add(path, n.line, "note: " .. n.list)
    for i, line in ipairs(n.prelude and n.prelude.at or {}) do
      add(path, line, string.format("  shared prefix %d/%d", i, n.prelude.count))
    end
  end
  -- Findings, then duplicate notes (`note: same body as ...`).
  local function findings(notes)
    for _, f in ipairs(text.by_line(fl)) do
      if (f.note or false) == notes then
        add(path, f.line, notes and text.dup_line(f) or text.finding_note(f, g.path).list)
        for _, row in ipairs(f.tests and text.table_rows(f, g.path) or {}) do
          add(row.path, row.line, row.text)
        end
        local label = f.kind == "duplicate" and "copy" or "impl"
        for _, o in ipairs(f.others) do
          local name = f.kind == "duplicate" and text.fn_name(o.name) or text.short(o.name)
          add(o.path, o.line, string.format("  %s: %s · %s", label, name, text.where(o, g.path)))
        end
      end
    end
  end
  findings(false)
  findings(true)
  local function show()
    if not vim.api.nvim_win_is_valid(win) then
      return
    end
    if #items == 0 then
      return text.say(vim.fs.basename(path) .. ": no smell culprits or findings")
    end
    -- A new list on the quickfix stack (:colder brings the previous back);
    -- step through it with :cnext / :cprevious.
    vim.fn.setqflist({}, " ", { title = "bloaty: " .. vim.fn.fnamemodify(path, ":~:.") .. (g.smell and g.smell.level and " (" .. g.smell.level .. ")" or ""), items = items })
    vim.api.nvim_win_call(win, function()
      vim.cmd("copen")
    end)
  end
  if (g.panics or 0) == 0 then
    return show()
  end
  bloaty.metrics(path, function(m)
    for _, fn in ipairs(m and m.functions or {}) do
      for _, s in ipairs(fn.panic_sites or {}) do
        -- bloaty counts characters; the list wants bytes.
        local col = s.col
        if vim.api.nvim_buf_is_valid(buf) then
          local line = vim.api.nvim_buf_get_lines(buf, s.line - 1, s.line, false)[1]
          local ok, byte = pcall(vim.str_byteindex, line or "", "utf-32", s.col - 1, false)
          col = ok and byte + 1 or col
        end
        table.insert(items, {
          filename = path, lnum = s.line, col = col,
          text = string.format("panic: %s in %s", s.kind, text.fn_name(fn.name)),
        })
      end
    end
    show()
  end)
end

-- The float's lines for `fn` (a `bloaty metrics` fn) of a file with
-- summary group `g` and findings `fl`.
local function info_lines(fn, g, fl)
  local lines = {
    text.fn_name(fn.name),
    text.complexity(fn),
    string.format("%d lines · %d params · %d clones", fn.lines, fn.params, fn.clones),
  }
  vim.list_extend(lines, text.shape(fn))
  if fn.panics > 0 then
    local at = {}
    for _, s in ipairs(fn.panic_sites or {}) do
      table.insert(at, s.kind .. " :" .. s.line)
    end
    table.insert(lines, string.format("%d panics: %s", fn.panics, table.concat(at, ", ")))
  else
    table.insert(lines, "no panics")
  end
  for _, c in ipairs(text.culprits(g, true)) do
    if c.fn_line == fn.line then
      table.insert(lines, string.format("◂ %s %s: why the file is %s (%s)", c.verdict, c.ev, g.smell.level, c.dim))
    end
  end
  for _, n in ipairs(text.notes(g)) do
    if n.line == fn.line then
      table.insert(lines, string.format("◂ %s · %s: noted, not counted", n.verdict, n.ev))
    end
  end
  for _, f in ipairs(fl) do
    if fn.start_line <= f.line and f.line <= fn.end_line then
      local line = f.kind == "duplicate" and text.dup_line(f) or text.finding_note(f, g.path).list
      table.insert(lines, "◂ " .. line)
    end
  end
  return lines
end

-- For a test (bloaty doesn't measure them): the table-driven-test
-- candidates it is a row of, under its name.
local function covering_lines(row, fl, this)
  local lines = {}
  for _, f in ipairs(text.by_line(fl)) do
    for i, t in ipairs(f.tests or {}) do
      if t.start_line <= row and row <= t.end_line then
        if #lines == 0 then
          table.insert(lines, text.fn_name(t.name))
        end
        table.insert(lines, "◂ " .. text.finding_note(f, this).list)
        table.insert(lines, string.format("row %d/%d", i, #f.tests))
        break
      end
    end
  end
  return lines
end

--- A float for the fn under the cursor: its numbers, shape, panic sites,
--- findings and whether it's the file's smell culprit. Closes on cursor
--- move.
function M.info()
  local buf, path, g, fl = current()
  if not buf then
    return
  end
  local win = vim.api.nvim_get_current_win()
  local row = vim.api.nvim_win_get_cursor(win)[1]
  local modified = vim.bo[buf].modified
  bloaty.metrics(path, function(m)
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
    local lines = best and info_lines(best, g, fl) or covering_lines(row, fl, g.path)
    if #lines == 0 then
      return text.say("no measured fn under the cursor (tests aren't measured)")
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
    BloatyLensNoted = "BloatySmellNoted",
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
    if r == nil and not bloaty.enabled() then
      -- Toggled off: also clear hidden buffers.
      for _, buf in ipairs(vim.api.nvim_list_bufs()) do
        clear(buf)
      end
    end
    render_visible()
  end)
end

return M
