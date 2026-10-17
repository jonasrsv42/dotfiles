-- bloaty's neo-tree column (`filesystem.components.bloaty` in
-- lua/mneotree.lua) and its `B` explain; see lua/mbloaty/init.lua.

local bloaty = require("mbloaty")
local text = require("mbloaty.text")

local M = {}

local function human(n)
  if n < 1000 then
    return tostring(n)
  elseif n < 9950 then
    return string.format("%.1fk", n / 1000)
  end
  return string.format("%dk", math.floor(n / 1000 + 0.5))
end

-- Size is information, not a finding: a dim line count, and a dim bar for a
-- node's share of the directory it's in (files and directories alike), so
-- where the code is shows at whatever level you look. The bar is blank where it would say nothing: under
-- 20 lines, the root row, or the only child with lines (always 100%).
-- Never a full block: the gap at the top keeps stacked rows from merging
-- into one long bar.
local BARS = { { 0.3, "▆" }, { 0.08, "▄" }, { 0.03, "▂" } }

local function bar(r, key, n, depth)
  local parent = vim.fs.dirname(key)
  if n < 20 or depth == 1 or (r.kids[parent] or 0) < 2 then
    return " "
  end
  for _, b in ipairs(BARS) do
    if n >= b[1] * r.loc[parent] then
      return b[2]
    end
  end
  return " "
end

--- neo-tree component: (config, node, state, remaining width).
function M.component(_, node, state, width)
  if not bloaty.enabled() or not state.path or not node.path then
    return {}
  end
  local r = bloaty.root(state.path)
  if r.skip or not r.ready then
    return {}
  end
  local key = r.real .. node.path:sub(#state.path + 1)
  local n = r.loc[key]
  if not n then
    return {}
  end
  -- When the name leaves no room, neo-tree would cut this column from the
  -- left (marks first): drop it whole instead. 9 columns + 1 padding, always
  -- all of them so the counts and bars line up.
  if width and vim.api.nvim_strwidth(node.name or "") + 10 > width then
    return {}
  end
  local level = r.smell[key]
  local depth = node:get_depth()
  -- The root row would always be hot: leave it plain.
  if depth == 1 then
    level = nil
  end
  return {
    { text = r.marks[key] and "•" or " ", highlight = "BloatyMark" },
    { text = level and "●" or " ", highlight = text.DOT_HL[level] or "BloatySmellWarm" },
    { text = string.format("%5s", human(n)), highlight = "BloatySize" },
    { text = bar(r, key, n, depth) .. " ", highlight = "BloatySize" },
  }
end

--- Explains the node under the cursor in a neo-tree window (mapped to `B`).
function M.explain(state)
  local node = state.tree:get_node()
  local r = state.path and bloaty.known(state.path)
  if not node or not node.path then
    return
  end
  if not r or r.skip then
    return text.say("not a Rust project (bloaty only measures cargo crates)")
  elseif not r.ready then
    return text.say("still computing…")
  end
  local key = r.real .. node.path:sub(#state.path + 1)
  local g = r.files[key]
  if node.type ~= "directory" and not g then
    return text.say(node.name .. ": not Rust code bloaty measures")
  end
  local lines = { string.format("%s: %s impl lines", node.name, r.loc[key] or 0) }
  if g then
    table.insert(lines, string.format("tests %d, docs %d, comments %d", g.test, g.doc, g.comment))
    if g.largest_fn then
      table.insert(lines, string.format("largest fn: %s (%d lines)", g.largest_fn.name, g.largest_fn.lines))
    end
    local cog = g.max_cognitive_complexity
    if cog then
      table.insert(lines, string.format("max cognitive complexity: %d (%s)", cog.value, cog.name))
    end
    local cc, n = g.max_cyclomatic_complexity, g.max_nesting
    if cc and n then
      table.insert(lines, string.format("max cyclomatic complexity: %d (%s) · max nesting: %d (%s)", cc.value, cc.name, n.value, n.name))
    end
    if (g.panics or 0) > 0 then
      table.insert(lines, string.format("panic sites: %d", g.panics))
    end
    local level = text.level(g)
    if level then
      table.insert(lines, string.format("%s: %s", level, g.smell.why or ""))
      -- Dispatches' shared prefixes (`why` only names the dispatch).
      for _, n in ipairs(text.notes(g)) do
        if n.prelude then
          table.insert(lines, string.format("%s: %s", n.name, text.prelude_long(n.prelude)))
        end
      end
    end
  elseif r.smell[key] == "hot" then
    table.insert(lines, "contains hot files")
  end
  if r.marks[key] then
    table.insert(lines, string.format("%d bloat findings (bloaty findings %s)", r.marks[key], node.path))
  end
  -- A file's duplicates with their facts, findings before notes.
  for _, notes in ipairs({ false, true }) do
    for _, f in ipairs(text.by_line(r.findings[key])) do
      if f.dup and f.note == notes then
        table.insert(lines, text.dup_line(f))
      end
    end
  end
  -- Table-driven-test and fixture candidates: a file's with their tests, a
  -- directory's counted.
  local tables = r.tables and r.tables[key]
  local setups = r.setups and r.setups[key]
  if node.type == "directory" then
    for _, c in ipairs({ { tables, "table-driven-test", "parametrize" }, { setups, "fixture", "fixture" } }) do
      if c[1] then
        table.insert(lines, string.format("%d %s %s (bloaty findings --kind %s %s)",
          c[1], c[2], c[1] == 1 and "candidate" or "candidates", c[3], node.path))
      end
    end
  elseif tables or setups then
    for _, f in ipairs(text.by_line(r.findings[key])) do
      if f.tests then
        local names = {}
        for i, t in ipairs(f.tests) do
          if i > 5 then
            table.insert(names, "+" .. (#f.tests - 5))
            break
          end
          table.insert(names, text.fn_name(t.name))
        end
        table.insert(lines, text.finding_note(f, key).list .. " · " .. table.concat(names, ", "))
      end
    end
  end
  text.say(table.concat(lines, "\n"))
end

return M
