-- bloaty's words: the formatters shared by the tree's `B` explain
-- (mbloaty.tree), the lens, its quickfix list and its float
-- (mbloaty.lens). Pure functions of bloaty's JSON (schema 3).

local M = {}

M.OUTDATED = "bloaty binary is outdated: cargo install --locked --path ~/dotfiles/tools/bloaty"

function M.say(msg)
  vim.notify(msg, vim.log.levels.INFO, { title = "bloaty" })
end

-- Highlights of a smell level: the tree's dot and the lens's verdicts.
M.DOT_HL = { hot = "BloatySmellHot", warm = "BloatySmellWarm", noted = "BloatySmellNoted" }
M.LENS_HL = { hot = "BloatyLensHot", warm = "BloatyLensWarm", noted = "BloatyLensNoted" }

-- A summary group's smell level ("hot", "warm", "noted") or nil.
function M.level(g)
  local s = g and g.smell
  return s and type(s.level) == "string" and s.level or nil
end

-- `Node<A, B>::f` -> `Node<…>::f`; `<Node<A> as Build>::f` -> `<Node<…> as
-- Build>::f` (only generic arguments collapse).
function M.short(name)
  local s = (name or "?"):gsub("([%w_])%b<>", "%1<…>")
  return s
end

-- A fn's name as shown: short, with `()` so a name never reads as a word
-- (`copy: same() · named.rs:134`).
function M.fn_name(name)
  return M.short(name) .. "()"
end

-- An arm's pattern with each path cut to its last two segments
-- (`crate::a::b::Foo::Quux` -> `Foo::Quux`).
function M.short_arm(name)
  local s = (name or "?"):gsub("[%w_]+::[%w_:]+", function(p)
    local segs = vim.split(p, "::", { plain = true })
    return #segs > 2 and (segs[#segs - 1] .. "::" .. segs[#segs]) or p
  end)
  return s
end

-- Between a verdict and its evidence: a space after `split?` / `panics:`,
-- else a dot (`duplicate · pollen/sys.rs:26`).
function M.sep(verdict)
  local last = verdict:sub(-1)
  return (last == "?" or last == ":") and " " or " · "
end

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

function M.where(o, this)
  local r = rel(o.path, this)
  return r == "line" and ("line " .. o.line) or (r .. ":" .. o.line)
end

-- "3 unwrap, 1 expect" from panic sites.
function M.kinds(sites)
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

-- A dispatch's shared prefix: several arms start with the same statements
-- ({statements, count, at}, bloaty's `repeat`). Short, for the lens: "shared
-- prefix in 4 arms (2 statements)"; long, for the floats: what it is and
-- what to do about it. `at` holds the first few lines: "27, 35, …".
local function statements(n)
  return n == 1 and "1 statement" or n .. " statements"
end

function M.prelude(rp)
  return string.format("shared prefix in %d arms (%s)", rp.count, statements(rp.statements))
end

function M.prelude_long(rp)
  return string.format(
    "shared prefix: %d arms start with the same %s (lines %s); hoist it above the match or into a helper",
    rp.count, statements(rp.statements), M.prelude_lines(rp))
end

function M.prelude_lines(rp)
  return table.concat(rp.at, ", ") .. (rp.count > #rp.at and ", …" or "")
end

-- What an excused fn is excused as ({kind, ...}): verdict and evidence.
function M.excuse(x)
  if x.kind == "table" then
    return "table", x.entries .. " entries"
  elseif x.kind == "dispatch" then
    local ev = x.arms .. " arms"
    if x["repeat"] then
      ev = ev .. " · " .. M.prelude(x["repeat"])
    end
    return "dispatch", ev
  end
  return "allowed", x.lint or "?"
end

-- The smell culprits, in order: the real (not excused) signals in
-- `smell.dims`, those bloaty marks `culprit` only unless `all`. A
-- dispatch's arm takes the blame when bloaty names one.
-- Each: { line, verdict, ev, merged (evidence when joining another
-- culprit's line), list (quickfix text), dim, fn_line, sites }.
function M.culprits(g, all)
  local s = g and g.smell
  if not (M.level(g) and type(s.dims) == "table") then
    return {}
  end
  local by = {}
  for _, d in ipairs(s.dims) do
    if not d.excused then
      by[d.metric] = d
    end
  end
  local out = {}
  -- At a dispatch's arm (bloaty blames one): the fn's number, then the
  -- arm's own (`100 lines · arm Listen 96`); merged into another culprit's
  -- line, just `(arm 96)` when it differs.
  local function add(d, verdict, ev, dim)
    local line, merged = d.line, ev
    if d.arm then
      line = d.arm.line
      if d.arm.value ~= d.value then
        merged = string.format("%s (arm %d)", ev, d.arm.value)
      end
      ev = string.format("%s · arm %s %d", ev, M.short_arm(d.arm.name), d.arm.value)
    end
    table.insert(out, {
      line = line, verdict = verdict, ev = ev, merged = merged,
      list = string.format("%s %s: %s", verdict, M.fn_name(d.name), ev), dim = dim, fn_line = d.line,
    })
  end
  local f, il = by.largest_fn, by.impl_lines
  if f and (all or f.culprit) then
    add(f, "split?", f.value .. " lines", "size")
  elseif il then
    local ev = il.value .. " impl lines"
    table.insert(out, {
      line = 1, verdict = "big file?", ev = ev, merged = ev, list = "big file? " .. ev, dim = "size",
    })
  end
  local c = by.cyclomatic_complexity
  if c and (all or c.culprit) then
    add(c, "simplify?", "cyclomatic complexity " .. c.value, "complexity")
  end
  local n = by.nesting
  if n and (all or n.culprit) then
    add(n, "flatten?", "nesting " .. n.value, "complexity")
  end
  local p = g.most_panics
  if by.panic_density and p then
    local ev = M.kinds(p.sites)
    if (g.panics or 0) > p.value then
      ev = ev .. " · " .. g.panics .. " in file"
    end
    table.insert(out, {
      line = p.line, verdict = "panics:", ev = ev, merged = "panics: " .. M.kinds(p.sites),
      list = string.format("panics: %s: %s (%d in file)", M.fn_name(p.name), M.kinds(p.sites), g.panics or 0),
      dim = "fragility", sites = p.sites, fn_line = p.line,
    })
  end
  return out
end

-- The excused fns among `smell.dims` (what a noted file was noted for; a
-- hot or warm file can have them too), one per fn and excuse, in order.
-- Each: { line, verdict, ev, list, noted = true, prelude ({statements,
-- count, at}) }.
function M.notes(g)
  local s = g and g.smell
  local out, seen = {}, {}
  for _, d in ipairs(s and type(s.dims) == "table" and s.dims or {}) do
    local x = d.excused
    local key = x and (d.line .. ":" .. x.kind .. ":" .. (x.lint or ""))
    if x and not seen[key] then
      seen[key] = true
      local verdict, ev = M.excuse(x)
      table.insert(out, {
        line = d.line, verdict = verdict, ev = ev, noted = true, name = d.name,
        list = string.format("%s %s: %s", verdict, M.fn_name(d.name), ev),
        prelude = x["repeat"],
      })
    end
  end
  return out
end

-- Signature parts as bloaty names them (`differs`) -> words.
local PART = {
  self_type = "Self type", impl_generics = "impl generics", trait = "trait", receiver = "receiver", generics = "generics",
  params = "parameters", ["return"] = "return type", qualifiers = "qualifiers",
}

-- "a and b", "a, b and c".
local function both(list)
  if #list <= 1 then
    return list[1] or ""
  end
  return table.concat(list, ", ", 1, #list - 1) .. " and " .. list[#list]
end

-- A duplicate group's facts (`findings` JSON), one line: "identical in
-- credits and cloudocol · credits depends on cloudocol · <hint>".
function M.dup_facts(d)
  local what = d.identical == false and "same shape" or "identical"
  if d.rule == "cfg_variants" or d.rule == "cfg_identical" then
    what = what .. " under different cfgs"
  end
  local parts = { what .. ((d.crates and #d.crates > 1) and (" in " .. both(d.crates)) or "") }
  if d.differs and #d.differs > 0 then
    local words = vim.tbl_map(function(p) return PART[p] or p end, d.differs)
    table.insert(parts, "signatures differ: " .. table.concat(words, ", "))
  end
  for _, dep in ipairs(d.depends or {}) do
    table.insert(parts, dep[1] .. " depends on " .. dep[2])
  end
  if d.hint then
    table.insert(parts, d.hint)
  end
  return table.concat(parts, " · ")
end

-- A duplicate's copies by signature class: "A::f(), A::g() | B::f()".
local function classes(d)
  local by = {}
  for _, c in ipairs(d.copies or {}) do
    local k = (c.class or 0) + 1
    by[k] = by[k] or {}
    table.insert(by[k], M.fn_name(c.name))
  end
  return table.concat(vim.tbl_map(function(names)
    return table.concat(names, ", ")
  end, by), " | ")
end

-- A file's findings entry (see init.lua `file_findings`) for lists and
-- explanations: "duplicate: f() · <facts>", "note: A::f() | B::f() · ...".
function M.dup_line(f)
  local head = f.note and ("note: " .. classes(f.dup)) or ("duplicate: " .. M.fn_name(f.name))
  return head .. " · " .. M.dup_facts(f.dup)
end

-- A finding as a note: { line, verdict, ev, list }.
function M.finding_note(f, this)
  local note = { line = f.line }
  if f.kind == "duplicate" then
    local o = f.others[1] or {}
    local identical = (f.dup or f).identical
    note.verdict = identical == false and "near-duplicate" or "duplicate"
    if o.name and o.name ~= f.name then
      note.verdict = note.verdict .. " of " .. M.fn_name(o.name)
    end
    note.ev = o.path and M.where(o, this) or nil
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
    note.ev = o and M.short(o.name) or nil
  else
    note.verdict = f.kind
  end
  note.list = (f.kind == "duplicate" and "" or (M.short(f.name) .. ": "))
    .. note.verdict .. (note.ev and (M.sep(note.verdict) .. note.ev) or "")
  return note
end

-- A copy of a file's findings, by line.
function M.by_line(findings)
  local sorted = vim.deepcopy(findings or {})
  table.sort(sorted, function(a, b)
    return a.line < b.line
  end)
  return sorted
end

-- Lines describing a `bloaty metrics` fn's shape and allowed lints, for
-- the float.
function M.shape(fn)
  local lines, sh = {}, fn.shape
  if sh and sh.kind == "table" then
    table.insert(lines, string.format("table · %d entries (its %d lines excused)", sh.entries, sh.lines))
  elseif sh and sh.kind == "dispatch" then
    local w, r, c = sh.worst, sh.remainder, sh.counted
    table.insert(lines, string.format("dispatch · %d arms, %d counted (%d lines · cyclomatic complexity %d), the small ones excused",
      sh.arms, c.arms, c.lines, c.cyclomatic_complexity))
    table.insert(lines, string.format("the rest: %d lines · cyclomatic complexity %d · nesting %d", r.lines, r.cyclomatic_complexity, r.nesting))
    local blamed = {}
    for _, k in ipairs({ { "lines", "%d lines" }, { "cyclomatic_complexity", "cyclomatic complexity %d" }, { "nesting", "nesting %d" } }) do
      local a = w[k[1]]
      if a then
        table.insert(blamed, string.format(k[2] .. " %s :%d", a.value, M.short_arm(a.name), a.line))
      end
    end
    if #blamed > 0 then
      table.insert(lines, "worst arms: " .. table.concat(blamed, " · "))
    end
    local rp = sh["repeat"]
    if rp then
      table.insert(lines, M.prelude_long(rp))
    end
  end
  for _, lint in ipairs(fn.allowed or {}) do
    table.insert(lines, "allows clippy::" .. lint)
  end
  return lines
end

return M
