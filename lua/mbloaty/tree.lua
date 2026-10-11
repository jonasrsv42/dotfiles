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
    { text = level and "●" or " ", highlight = text.DOT_HL[level] or "BloatySmellWarm" },
    { text = string.format("%5s ", human(n)), highlight = heat(node, n, r, key) },
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
    local cc = g.max_cyclomatic_complexity
    if cc then
      table.insert(lines, string.format("max cyclomatic complexity: %d (%s)", cc.value, cc.name))
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
  text.say(table.concat(lines, "\n"))
end

return M
