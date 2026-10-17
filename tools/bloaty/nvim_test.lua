-- Headless test of the Neovim side of bloaty (lua/mbloaty/init.lua: runs and
-- cache; mbloaty/tree.lua: neo-tree column; mbloaty/lens.lua: in-file lens,
-- quickfix, float; mbloaty/text.lua: their words) against the golden
-- fixture workspace (tests/fixtures/ws, copied to a temp dir). From the
-- repository:
--
--   nvim --headless -u NONE -l tools/bloaty/nvim_test.lua
--
-- (`cargo test` runs it too, via tests/nvim.rs, when nvim is on PATH.)
--
-- Uses `$BLOATY` if set (a binary named bloaty), else builds tools/bloaty
-- (`cargo build --offline`, honouring CARGO_TARGET_DIR) and puts the built
-- binary's directory first on PATH. Prints `ok` lines and `all N checks
-- passed` (exit 0), or `FAIL: ...` at the first failure (exit 1); either
-- way the temp copy is removed. Needs neither neo-tree (the column is called
-- with fake nodes) nor any user config.

local here = vim.fs.dirname(vim.fs.normalize(vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p")))
local dotfiles = vim.fs.dirname(vim.fs.dirname(here))
vim.opt.rtp:prepend(dotfiles)
vim.g.mapleader = " "

-- Fails the run: caught by the xpcall around `main`, which cleans up.
local function die(msg)
  error({ fail = msg }, 0)
end

local checks = 0
local function ok(msg)
  checks = checks + 1
  io.stdout:write("ok   " .. msg .. "\n")
end

local function eq(got, want, what)
  if not vim.deep_equal(got, want) then
    die(string.format("%s\n  want: %s\n  got:  %s", what, vim.inspect(want), vim.inspect(got)))
  end
  ok(what)
end

local function sh(cmd, cwd)
  local r = vim.system(cmd, { cwd = cwd, text = true }):wait()
  if r.code ~= 0 then
    die(table.concat(cmd, " ") .. ": " .. (r.stderr or ""))
  end
  return r.stdout
end

-- The directory of the bloaty binary under test: `$BLOATY`'s, else the
-- one cargo just built (wherever CARGO_TARGET_DIR puts it).
local function binary_dir()
  if vim.env.BLOATY and vim.env.BLOATY ~= "" then
    return vim.fs.dirname(vim.fn.fnamemodify(vim.env.BLOATY, ":p"))
  end
  local out = sh({ "cargo", "build", "--offline", "--quiet", "--message-format=json" }, here)
  for _, line in ipairs(vim.split(out, "\n", { trimempty = true })) do
    local okj, msg = pcall(vim.json.decode, line, { luanil = { object = true } })
    if okj and msg.reason == "compiler-artifact" and msg.target and msg.target.name == "bloaty"
        and type(msg.executable) == "string" then
      return vim.fs.dirname(msg.executable)
    end
  end
  die("cargo build reported no bloaty executable")
end

local tmp = vim.fn.tempname()
local base_path = vim.env.PATH

-- Shared by the sections below.
local ws, bin_dir, mb, tree, lens
local notes = {}
local state = {}
local ns = vim.api.nvim_create_namespace("bloaty_lens")

-- (Re)loads the plugin from scratch, as a fresh Neovim would.
local function load()
  for _, m in ipairs({ "mbloaty", "mbloaty.lens", "mbloaty.tree", "mbloaty.text" }) do
    package.loaded[m] = nil
  end
  mb = require("mbloaty")
  mb.setup()
  tree, lens = require("mbloaty.tree"), require("mbloaty.lens")
end

-- A neo-tree node for `rel` in the tree rooted at `state.path`.
local function node(rel, type)
  local depth = rel == "" and 1 or #vim.split(rel, "/") + 1
  return {
    path = rel == "" and state.path or (state.path .. "/" .. rel),
    name = rel == "" and vim.fs.basename(state.path) or vim.fs.basename(rel),
    type = type or "file",
    get_depth = function()
      return depth
    end,
  }
end

local function column(rel, type)
  local parts = tree.component({}, node(rel, type), state, 200)
  return vim.tbl_map(function(p)
    return p.text
  end, parts)
end

-- Extmark texts of the current buffer, { [line] = text }, and the
-- highlight of each mark's verdict, { [line] = group }.
local function marks()
  local out, hls = {}, {}
  for _, m in ipairs(vim.api.nvim_buf_get_extmarks(0, ns, 0, -1, { details = true })) do
    local t = ""
    for _, c in ipairs(m[4].virt_text) do
      t = t .. c[1]
    end
    out[m[2] + 1] = t
    hls[m[2] + 1] = m[4].virt_text[2] and m[4].virt_text[2][2]
  end
  return out, hls
end

local function wait(what, cond)
  if not vim.wait(20000, cond, 20) then
    die("timed out waiting for " .. what)
  end
end

-- The quickfix list `lens.qflist()` makes for `rel`, as "line text".
local function qf(rel)
  vim.cmd.edit(rel)
  lens.qflist()
  wait(rel .. " quickfix", function()
    return vim.startswith(vim.fn.getqflist({ title = 1 }).title, "bloaty: " .. rel)
  end)
  local items = vim.tbl_map(function(it)
    return it.lnum .. " " .. it.text
  end, vim.fn.getqflist())
  vim.cmd.cclose()
  return items
end

local function setup()
  bin_dir = binary_dir()
  if vim.fn.executable(bin_dir .. "/bloaty") ~= 1 then
    die("no executable bloaty in " .. bin_dir)
  end
  vim.env.PATH = bin_dir .. ":" .. base_path
  -- A private copy of the fixture: in place, tests/fixtures/.bloatyignore
  -- hides it from bloaty. Its own (empty) .git keeps a repository or
  -- Cargo.toml around the temp dir from moving the workspace root.
  ws = tmp .. "/ws"
  sh({ "mkdir", "-p", tmp })
  sh({ "cp", "-r", here .. "/tests/fixtures/ws", ws })
  sh({ "mkdir", ws .. "/.git" })
  ws = vim.uv.fs_realpath(ws)
  vim.cmd.cd(ws)
  vim.notify = function(msg, level)
    table.insert(notes, { msg = tostring(msg), level = level })
  end
  state.path = ws
  load()
end

local function test_column()
  -- Column: first call starts the runs, later ones read the cache.
  eq(column("core/src/hot.rs"), {}, "column empty while computing")
  wait("the root's runs", function()
    return #column("core/src/hot.rs") > 0
  end)
  -- The bar is the node's share of its directory's impl lines (core/src:
  -- 858): hot.rs 80 (9.3%), routes.rs 75 (8.7%), badarm.rs / long.rs 43
  -- (5.0%), shapes.rs 24 (2.8%), f01.rs 13 (under 20).
  eq(column("core/src/hot.rs"), { " ", "●", "   80", "▄ " }, "column: hot file has its bar and dot")
  eq(column("core/src/shapes.rs"), { "•", " ", "   24", "  " }, "column: findings mark on shapes.rs (under 3%: no bar)")
  eq(column("core/src/badarm.rs"), { " ", "●", "   43", "▂ " }, "column: warm file has a dot")
  eq(column("core/src/routes.rs"), { " ", "●", "   75", "▄ " }, "column: noted file has a dot")
  local function hl(rel, i)
    return tree.component({}, node(rel), state, 200)[i].highlight
  end
  eq({ hl("core/src/hot.rs", 2), hl("core/src/badarm.rs", 2), hl("core/src/routes.rs", 2) },
    { "BloatySmellHot", "BloatySmellWarm", "BloatySmellNoted" }, "column: dot colours hot / warm / noted")
  eq(column("core/src/long.rs"), { " ", " ", "   43", "▂ " }, "column: a long fn under the bar is plain")
  eq(column("core/src/f01.rs"), { " ", " ", "   13", "  " }, "column: under 20 lines: no bar")
  eq(column("core/src", "directory"), { "•", "●", "  858", "▆ " }, "column: directory inherits hot, bar by share (858 of core's 873)")
  eq(column("app", "directory"), { "•", " ", "   63", "▂ " }, "column: directory bar by share (63 of 936)")
  eq(column("app/src/main.rs"), { " ", " ", "   38", "▆ " }, "column: file bar by share (38 of app/src's 49)")
  eq(column(""), { "•", " ", "  936", "  " }, "column: root row never gets a dot or a bar")
  -- Size is information: never a diagnostic colour, whatever the share.
  for _, rel in ipairs({ "core/src/hot.rs", "core/src/f01.rs", "app/src/main.rs", "core/src", "" }) do
    local h = hl(rel, 3) == hl(rel, 4) and hl(rel, 4)
    if h ~= "BloatySize" or vim.fn.synIDattr(vim.fn.synIDtrans(vim.fn.hlID(h)), "name"):find("^Diagnostic") then
      die("column: size of " .. rel .. " uses " .. tostring(h))
    end
  end
  ok("column: size count and bar are BloatySize, never Diagnostic*")
  eq(vim.api.nvim_get_hl(0, { name = "BloatySize" }).link, "NeoTreeDimText", "column: BloatySize links to neo-tree's dim text")
  local widths = {}
  for _, rel in ipairs({ "core/src/hot.rs", "core/src/shapes.rs", "core/src/f01.rs", "core/src", "" }) do
    table.insert(widths, vim.fn.strwidth(table.concat(column(rel, rel:find("%.rs$") and nil or "directory"))))
  end
  eq(widths, { 9, 9, 9, 9, 9 }, "column: same width on every row, so counts and bars line up")
  local narrow = tree.component({}, node("core/src/hot.rs"), state, #"hot.rs" + 9)
  eq(narrow, {}, "column: dropped whole when the name leaves no room")
  eq(#tree.component({}, node("core/src/hot.rs"), state, #"hot.rs" + 10), 4, "column: shown with 10 columns to spare")
  eq(column("Cargo.toml"), {}, "column: nothing on non-Rust files")
  eq(column("core/tests/table.rs"), {}, "column: no mark for table-driven-test candidates (test code)")
  eq(column("core/tests/setup.rs"), {}, "column: no mark for fixture candidates (test code)")

  -- An only child with lines gets no bar (it would always be full): src in
  -- a crate of src/{lib,a,b,c}.rs, whose files share 103 lines.
  local lone = tmp .. "/lone"
  sh({ "mkdir", "-p", lone .. "/src", lone .. "/.git" })
  vim.fn.writefile({ "[package]", 'name = "lone"', 'version = "0.1.0"', 'edition = "2021"' }, lone .. "/Cargo.toml")
  vim.fn.writefile({ "mod a;", "mod b;", "mod c;" }, lone .. "/src/lib.rs")
  for name, n in pairs({ a = 60, b = 30, c = 10 }) do
    local body = { "pub fn " .. name .. "() -> u32 {", "    let mut x = 0;" }
    for i = 1, n - 4 do
      table.insert(body, "    x += " .. i .. ";")
    end
    vim.list_extend(body, { "    x", "}" })
    vim.fn.writefile(body, lone .. "/src/" .. name .. ".rs")
  end
  lone = vim.uv.fs_realpath(lone)
  state.path = lone
  wait("the lone root's runs", function()
    return #column("src", "directory") > 0
  end)
  eq({ column("src", "directory")[4], column("src/a.rs")[4], column("src/b.rs")[4], column("src/c.rs")[4] },
    { "  ", "▆ ", "▄ ", "  " }, "column: only child has no bar; a.rs 60 / b.rs 30 of 103 (29%), c.rs under 20")
  state.path = ws

end

local function test_lens()
  -- Lens.
  vim.cmd.edit("core/src/hot.rs")
  wait("hot.rs lens", function()
    return next((marks())) ~= nil
  end)
  eq(marks(), { [11] = "◂ split? 71 lines · cognitive complexity 61" }, "lens: hot.rs culprits merge on plan's line")

  vim.cmd.edit("core/src/badarm.rs")
  eq(marks(), {
    [22] = "◂ simplify? cognitive complexity 17 · arm Op::Rebalance 16",
  }, "lens: a dispatch's bad arm is the culprit, marked on the arm (only culprits: no size verdict)")

  -- Noted: what was excused, in blue.
  local function noted(rel, want, what)
    vim.cmd.edit(rel)
    local texts, hls = marks()
    eq(texts, want, what)
    for line in pairs(want) do
      eq(hls[line], "BloatyLensNoted", what .. ": blue at line " .. line)
    end
  end
  noted("core/src/routes.rs", { [14] = "◂ table · 37 entries" }, "lens: a table")
  noted("core/src/allowed.rs", { [12] = "◂ allowed · too_many_lines" }, "lens: an allowed lint")
  -- The clean dispatch `weight` (line 87) gets nothing.
  noted("core/src/dispatch.rs", { [26] = "◂ dispatch · 8 arms · shared prefix in 4 arms (1 statement)" },
    "lens: a dispatch with a shared prefix")
  vim.cmd.edit("core/src/long.rs")
  eq(marks(), {}, "lens: no marks on a long fn under the bar")

  vim.cmd.edit("core/src/fragile.rs")
  local m = marks()
  local panics = m[18] or ""
  if not (panics:find("^◂ panics: ") and panics:find("1 assert!", 1, true) and panics:find("1 panic!", 1, true)
      and panics:find("7 in file", 1, true)) then
    die("lens: fragile.rs panics mark on line 18, got " .. vim.inspect(m))
  end
  eq(m[19], "◂ assert!", "lens: fragile.rs worst fn's panic sites")
  eq(m[21], "◂ panic!", "lens: fragile.rs worst fn's second site")

  vim.cmd.edit("core/src/shapes.rs")
  eq(marks(), { [3] = "◂ only impl: Square", [28] = "◂ unused" }, "lens: findings on shapes.rs")

  vim.cmd.edit("core/src/dup_a.rs")
  eq(marks(), {
    [3] = "◂ duplicate of adler() · src/dup_b.rs:3 +1",
    [96] = "◂ duplicate of blend() · line 105",
    [105] = "◂ duplicate of mix() · line 96",
  }, "lens: impl copies of finding-tier duplicates; the test copy (line 23), the notes (Left/Right::twice, spread) and plus/add (too short) are unmarked")

  vim.cmd.edit("app/src/dup_b.rs")
  eq(marks(), { [3] = "◂ duplicate of checksum() · src/dup_a.rs:3 +1" },
    "lens: a copy in another crate names one (at most two path components)")

  vim.cmd.edit("core/tests/it.rs")
  eq(marks(), {}, "lens: test code is never marked")

  vim.cmd.edit("core/src/f01.rs")
  eq(marks(), {}, "lens: no marks on a cool file")

  vim.cmd.edit("core/tests/table.rs")
  eq(marks(), {
    [11] = "◂ parametrize? 4 tests · 2 literals",
    [81] = "◂ parametrize? 2 tests · 5 literals",
  }, "lens: table-driven-test candidates, on each group's first test only")

  vim.cmd.edit("core/tests/setup.rs")
  eq(marks(), {
    [22] = "◂ fixture? 3 tests share 4 statements",
    [98] = "◂ fixture? 3 tests share 5 statements",
    [140] = "◂ fixture? 3 tests share 4 statements",
    [168] = "◂ fixture? 3 tests share 9 statements",
  }, "lens: fixture candidates, on each group's first test only")

end

local function test_quickfix()
  -- Quickfix.
  vim.cmd.edit("core/src/hot.rs")
  lens.qflist()
  wait("quickfix", function()
    return #vim.fn.getqflist() > 0
  end)
  local q = vim.fn.getqflist({ title = 1, items = 1 })
  eq(q.title, "bloaty: core/src/hot.rs (hot)", "quickfix title: the file and its level")
  eq(vim.tbl_map(function(it)
    return { it.lnum, it.text }
  end, q.items), {
    { 11, "split? plan(): 71 lines · cognitive complexity 61" },
    { 1, "also: 80 impl lines" },
  }, "quickfix: hot.rs culprits, merged on their line, then the rest without a verdict")
  vim.cmd.cclose()

  local texts = qf("core/src/fragile.rs")
  eq(#texts, 8, "quickfix: fragile.rs culprit + 7 panic sites")
  eq(texts[2], "19 panic: assert! in Registry::add()", "quickfix: panic sites from bloaty metrics")

  eq(qf("core/src/dispatch.rs"), {
    "26 note: dispatch run(): 8 arms · shared prefix in 4 arms (1 statement)",
    "29   shared prefix 1/4",
    "37   shared prefix 2/4",
    "45   shared prefix 3/4",
    "54   shared prefix 4/4",
  }, "quickfix: a noted dispatch and each copy of its shared prefix")

  eq(qf("core/src/dup_a.rs"), {
    "3 duplicate of adler() · src/dup_b.rs:3 +1",
    "3   copy: adler() · src/dup_b.rs:3",
    "23   copy: reference() · line 23",
    "23 duplicate of adler() · src/dup_b.rs:3 +1",
    "3   copy: adler() · src/dup_b.rs:3",
    "3   copy: checksum() · line 3",
    "96 duplicate of blend() · line 105",
    "105   copy: blend() · line 105",
    "105 duplicate of mix() · line 96",
    "96   copy: mix() · line 96",
    "73 note: Left::twice() | Right::twice() · identical · signatures differ: Self type"
      .. " · same body on different types: a shared helper or a generic impl may fit; often by design",
    "84   copy: Right::twice() · line 84",
    "84 note: Left::twice() | Right::twice() · identical · signatures differ: Self type"
      .. " · same body on different types: a shared helper or a generic impl may fit; often by design",
    "73   copy: Left::twice() · line 73",
    "116 note: spread(), spread_ref() · same shape",
    "136   copy: spread_ref() · line 136",
    "136 note: spread(), spread_ref() · same shape",
    "116   copy: spread() · line 116",
  }, "quickfix: duplicate findings with their copies, then notes with their facts")

  local claim = ": one test over a table (or a helper taking them)"
  eq(qf("core/tests/table.rs"), {
    "11 parametrize? 4 tests differ only in 2 of 4 literals, 2 lines each, 2 identical" .. claim,
    "11   row 1/4: clamps_below() · line 11",
    "16   row 2/4: clamps_above() · line 16",
    "21   row 3/4: keeps_inside() · line 21 · identical",
    "26   row 4/4: keeps_inside_again() · line 26 · identical",
    "81 parametrize? 2 tests differ only in 5 of 14 literals, 8 lines each" .. claim,
    "81   row 1/2: sums_pairs() · line 81",
    "92   row 2/2: sums_triples() · line 92",
  }, "quickfix: a table-driven-test candidate, then each of its tests")

  local setup = " lines): one setup fn they call"
  eq(qf("core/tests/setup.rs"), {
    "22 fixture? 3 tests open with the same 4 statements (4" .. setup,
    "22   shares 1/3: totals_three_rows() · line 22",
    "32   shares 2/3: removes_the_last_row() · line 32",
    "42   shares 3/3: clears_every_row() · line 42",
    "98 fixture? 3 tests open with the same 5 statements (5" .. setup,
    "98   shares 1/3: queue_drains_in_order() · line 98",
    "109   shares 2/3: queue_totals() · line 109",
    "119   shares 3/3: queue_keeps_the_last() · line 119",
    "140 fixture? 3 tests open with the same 4 statements (4" .. setup,
    "140   shares 1/3: log_boots() · line 140",
    "149   shares 2/3: log_loads() · line 149",
    "158   shares 3/3: log_runs() · line 158",
    "168 fixture? 3 tests open with the same 9 statements (9" .. setup,
    "168   shares 1/3: log_replays() · line 168",
    "182   shares 2/3: log_rotates() · line 182",
    "197   shares 3/3: log_trims() · line 197",
  }, "quickfix: fixture candidates, each with its tests")

  eq(qf("core/src/badarm.rs"), {
    "22 simplify? apply(): cognitive complexity 17 · arm Op::Rebalance 16",
    "22 also: apply: 28 lines · arm Op::Rebalance 24",
    "1 also: 43 impl lines",
  }, "quickfix: the bad arm, named; size isn't a culprit, so no verdict")
end

local function test_words()
  -- A same-shape duplicate reads as a near-duplicate (its facts are under
  -- `dup`, as init.lua's `file_findings` keeps them).
  local text = require("mbloaty.text")
  -- A long flat fn reads gentler than a split.
  local flat = text.culprits({ smell = { level = "warm", dims = {
    { dim = "size", metric = "largest_fn", value = 104, name = "steps", line = 43, culprit = true, flat = true },
  } } })
  eq({ flat[1].verdict, flat[1].ev, flat[1].list }, { "long?", "104 lines · flat", "long? steps(): 104 lines · flat" },
    "words: a long flat fn")
  local arm = text.culprits({ smell = { level = "warm", dims = {
    { dim = "size", metric = "largest_fn", value = 171, name = "handle_message", line = 17, culprit = true, flat = true,
      arm = { name = "ServerError::Listen", line = 90, value = 96 } },
    { dim = "complexity", metric = "cognitive_complexity", value = 6, name = "handle_message", line = 17 },
  } } }, true)
  eq({ arm[1].line, arm[1].verdict, arm[1].ev }, { 90, "long?", "arm ServerError::Listen 96 lines · flat (fn 171)" },
    "words: a long flat arm")
  eq({ #arm, arm[2].list }, { 2, "also: handle_message: cognitive complexity 6" }, "words: a dim that isn't a culprit")
  local f = {
    kind = "duplicate", name = "a", line = 1, dup = { identical = false },
    others = { { name = "b", path = "/x/b.rs", line = 2 } },
  }
  eq(text.finding_note(f, "/x/a.rs").list, "near-duplicate of b() · b.rs:2", "words: a near-duplicate")
  f.dup.identical = true
  eq(text.finding_note(f, "/x/a.rs").list, "duplicate of b() · b.rs:2", "words: an identical duplicate")
end

local function test_explain()
  -- `B` in the tree: a file's duplicates with their facts.
  local before = #notes
  tree.explain(vim.tbl_extend("force", state, {
    tree = { get_node = function() return node("app/src/dup_b.rs") end },
  }))
  local said = notes[before + 1] and notes[before + 1].msg or ""
  local want = "duplicate: adler() · identical in fxapp and fxcore · fxapp depends on fxcore"
    .. " · move it into a shared crate, or reuse it through a dependency"
  if not said:find(want, 1, true) then
    die("B: dup_b.rs facts\n  want: " .. want .. "\n  got:  " .. said)
  end
  ok("B: a duplicate's facts (crates and the dependency between them, no reuse claim)")
  want = "max cognitive complexity: 1 (adler)"
  if not said:find(want, 1, true) then
    die("B: dup_b.rs max cognitive\n  want: " .. want .. "\n  got:  " .. said)
  end
  ok("B: max cognitive complexity")
  for _, c in ipairs({ { "core/src/hot.rs", nil, "hot.rs: 80 impl lines" }, { "core/src", "directory", "src: 858 impl lines" } }) do
    local n = #notes
    tree.explain(vim.tbl_extend("force", state, {
      tree = { get_node = function() return node(c[1], c[2]) end },
    }))
    eq(vim.split(notes[n + 1] and notes[n + 1].msg or "", "\n")[1], c[3], "B: " .. c[1] .. " has its line count (not in the column)")
  end

  -- `B` on a test file lists its table-driven-test candidates; on a
  -- directory, counts them.
  local function explain(rel, type)
    local n = #notes
    tree.explain(vim.tbl_extend("force", state, {
      tree = { get_node = function() return node(rel, type) end },
    }))
    return notes[n + 1] and notes[n + 1].msg or ""
  end
  said = explain("core/tests/table.rs")
  want = "parametrize? 4 tests differ only in 2 of 4 literals, 2 lines each, 2 identical: one test over a table"
    .. " (or a helper taking them) · clamps_below(), clamps_above(), keeps_inside(), keeps_inside_again()"
  if not said:find(want, 1, true) then
    die("B: table.rs candidates\n  want: " .. want .. "\n  got:  " .. said)
  end
  ok("B: a file's table-driven-test candidates with their tests")
  said = explain("core/tests", "directory")
  want = "2 table-driven-test candidates (bloaty findings --kind parametrize " .. ws .. "/core/tests)"
  if not said:find(want, 1, true) then
    die("B: core/tests count\n  want: " .. want .. "\n  got:  " .. said)
  end
  ok("B: a directory counts its table-driven-test candidates")
  said = explain("core/tests/setup.rs")
  want = "fixture? 3 tests open with the same 9 statements (9 lines): one setup fn they call"
    .. " · log_replays(), log_rotates(), log_trims()"
  if not said:find(want, 1, true) then
    die("B: setup.rs candidates\n  want: " .. want .. "\n  got:  " .. said)
  end
  ok("B: a file's fixture candidates with their tests")
  said = explain("core/tests", "directory")
  want = "4 fixture candidates (bloaty findings --kind fixture " .. ws .. "/core/tests)"
  if not said:find(want, 1, true) then
    die("B: core/tests fixture count\n  want: " .. want .. "\n  got:  " .. said)
  end
  ok("B: a directory counts its fixture candidates")
end

local function test_info()
  -- <leader>bi on the dispatch: its shape and the note.
  vim.cmd.edit("core/src/dispatch.rs")
  vim.api.nvim_win_set_cursor(0, { 28, 0 })
  lens.info()
  local float
  wait("the info float", function()
    for _, w in ipairs(vim.api.nvim_list_wins()) do
      if vim.api.nvim_win_get_config(w).relative ~= "" then
        float = w
        return true
      end
    end
  end)
  local lines = vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(float), 0, -1, false)
  eq({ lines[1], lines[2], lines[3], lines[4], lines[5], lines[6], lines[8] }, {
    "run()",
    "cognitive complexity 11 (worst at line 29) · cyclomatic 7 · nesting 2",
    "60 lines · 2 params · 0 clones",
    "dispatch · 8 arms, 0 counted (0 lines · cognitive complexity 0), the small ones excused",
    "the rest: 4 lines · cognitive complexity 0 · cyclomatic 1 · nesting 0",
    "shared prefix: 4 arms start with the same 1 statement (lines 29, 37, 45, 54); hoist it above the match or into a helper",
    "◂ dispatch · 8 arms · shared prefix in 4 arms (1 statement): noted, not counted",
  }, "<leader>bi: a dispatch's complexity, shape and shared prefix")
  vim.api.nvim_win_close(float, true)

  -- <leader>bi on a duplicate note: its facts.
  vim.cmd.edit("core/src/dup_a.rs")
  vim.api.nvim_win_set_cursor(0, { 73, 0 })
  lens.info()
  float = nil
  wait("the duplicate's info float", function()
    for _, w in ipairs(vim.api.nvim_list_wins()) do
      if vim.api.nvim_win_get_config(w).relative ~= "" then
        float = w
        return true
      end
    end
  end)
  lines = vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(float), 0, -1, false)
  eq(lines[#lines], "◂ note: Left::twice() | Right::twice() · identical · signatures differ: Self type"
    .. " · same body on different types: a shared helper or a generic impl may fit; often by design",
    "<leader>bi: a duplicate note with its facts")
  vim.api.nvim_win_close(float, true)

  -- <leader>bi on a test (not measured): the findings covering it.
  vim.cmd.edit("core/tests/table.rs")
  vim.api.nvim_win_set_cursor(0, { 17, 0 })
  lens.info()
  float = nil
  wait("the test's info float", function()
    for _, w in ipairs(vim.api.nvim_list_wins()) do
      if vim.api.nvim_win_get_config(w).relative ~= "" then
        float = w
        return true
      end
    end
  end)
  lines = vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(float), 0, -1, false)
  eq(lines, {
    "clamps_above()",
    "◂ parametrize? 4 tests differ only in 2 of 4 literals, 2 lines each, 2 identical: one test over a table"
      .. " (or a helper taking them)",
    "row 2/4",
  }, "<leader>bi on a test: the candidate covering it and its row")
  vim.api.nvim_win_close(float, true)

  -- ...and on a test sharing a fixture candidate's opening.
  vim.cmd.edit("core/tests/setup.rs")
  vim.api.nvim_win_set_cursor(0, { 111, 0 })
  lens.info()
  float = nil
  wait("the fixture test's info float", function()
    for _, w in ipairs(vim.api.nvim_list_wins()) do
      if vim.api.nvim_win_get_config(w).relative ~= "" then
        float = w
        return true
      end
    end
  end)
  lines = vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(float), 0, -1, false)
  eq(lines, {
    "queue_totals()",
    "◂ fixture? 3 tests open with the same 5 statements (5 lines): one setup fn they call",
    "shares 2/3",
  }, "<leader>bi on a test: the fixture candidate covering it")
  vim.api.nvim_win_close(float, true)
  vim.cmd.edit("core/tests/table.rs")

  -- ...but not on a test helper (no candidate has it as a row).
  local before = #notes
  vim.api.nvim_win_set_cursor(0, { 4, 0 })
  lens.info()
  wait("the helper's message", function()
    return #notes > before
  end)
  eq(notes[#notes].msg, "no measured fn under the cursor (tests aren't measured)",
    "<leader>bi on a test helper: nothing claimed")
end

local function test_toggles()
  -- Toggles: <leader>bb (this buffer), <leader>bt (everything).
  vim.cmd.edit("core/src/hot.rs")
  local function keys(k)
    vim.api.nvim_feedkeys(vim.keycode(k), "x", false)
  end
  keys("<leader>bb")
  eq(marks(), {}, "<leader>bb hides the lens in this buffer")
  keys("<leader>bb")
  eq(next((marks())), 11, "<leader>bb shows it again")
  keys("<leader>bt")
  eq(marks(), {}, "<leader>bt hides the lens")
  eq(column("core/src/hot.rs"), {}, "<leader>bt hides the column")
  vim.cmd.edit("core/src/shapes.rs")
  eq(marks(), {}, "<leader>bt: other buffers stay clear")
  keys("<leader>bt")
  eq(next((marks())) ~= nil, true, "<leader>bt shows the lens again")
  eq(column("core/src/hot.rs")[2], "●", "<leader>bt shows the column again")

end

local function test_quiet()
  -- Non-Rust file: nothing, quietly.
  local before = #notes
  vim.cmd.edit("Cargo.toml")
  lens.render(0)
  vim.wait(200)
  eq(marks(), {}, "no marks on a non-Rust file")
  eq(#notes, before, "no notifications for a non-Rust file")
  eq(vim.tbl_filter(function(n)
    return n.level and n.level >= vim.log.levels.WARN
  end, notes), {}, "no warnings or errors during the run")

end

local function test_outdated()
  -- An outdated binary (JSON without `schema`): one warning, no data.
  local fake = tmp .. "/fakebin"
  sh({ "mkdir", "-p", fake })
  vim.fn.writefile({
    "#!/bin/sh",
    [[echo '{"root": "/", "groups": [{"name": "x", "path": "/x.rs", "impl": 1}], "findings": []}']],
  }, fake .. "/bloaty")
  sh({ "chmod", "+x", fake .. "/bloaty" })
  vim.env.PATH = fake .. ":" .. base_path
  vim.cmd("silent! %bwipeout!")
  load()
  notes = {}
  eq(column("core/src/hot.rs"), {}, "outdated binary: column empty while computing")
  wait("the outdated runs", function()
    return #notes > 0
  end)
  vim.wait(300)
  state.path = ws .. "/core"
  column("src/hot.rs")
  vim.wait(500)
  eq(#notes, 1, "outdated binary: notified once")
  if not notes[1].msg:find("outdated", 1, true) then
    die("outdated binary: unexpected message " .. notes[1].msg)
  end
  eq(column("src/hot.rs"), {}, "outdated binary: nothing shown")
  vim.wait(300)
  eq(column("src/hot.rs"), {}, "outdated binary: no rerun while the binary is unchanged")
  eq(#notes, 1, "outdated binary: still one warning")

  -- Reinstalled (another binary on PATH): the next lookup recomputes.
  vim.env.PATH = bin_dir .. ":" .. base_path
  column("src/hot.rs")
  wait("the recompute after reinstall", function()
    return #column("src/hot.rs") > 0
  end)
  eq(column("src/hot.rs")[2], "●", "after reinstall: the dot is back")
  -- Outdated again: warned again.
  vim.env.PATH = fake .. ":" .. base_path
  vim.cmd("BloatyRefresh")
  wait("a second warning", function()
    return #notes == 2
  end)
  eq(notes[2].msg, notes[1].msg, "outdated again after a good run: warned again")end

local function main()
  setup()
  test_column()
  test_lens()
  test_quickfix()
  test_words()
  test_explain()
  test_info()
  test_toggles()
  test_quiet()
  test_outdated()
end

local ok_run, err = xpcall(main, function(e)
  return type(e) == "table" and e.fail or debug.traceback(tostring(e))
end)
vim.cmd.cd("/")
vim.fn.delete(tmp, "rf")
-- nvim's own temp dir (tempname()'s parent), gone at exit anyway unless
-- os.exit skips that.
vim.fn.delete(vim.fn.fnamemodify(tmp, ":h"), "rf")
if not ok_run then
  io.stderr:write("FAIL: " .. err .. "\n")
  os.exit(1)
end
io.stdout:write(string.format("all %d checks passed\n", checks))
os.exit(0)
