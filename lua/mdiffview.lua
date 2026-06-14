-- diffview.nvim: review a whole changeset locally, cycling through files/hunks.
local actions = require("diffview.actions")

require("diffview").setup({
  keymaps = {
    -- In the diff windows:
    view = {
      { "n", "<leader>de", "<Cmd>wincmd b<CR>", { desc = "Jump to the editable (working-tree) pane" } },
      { "n", "<leader>db", actions.focus_files, { desc = "Back to browsing files (file panel)" } },
    },
    -- In the file-list panel: just press `e` on a file to open + edit it.
    -- (select_entry opens the diff; wincmd b lands on the editable working-tree pane.)
    file_panel = {
      {
        "n",
        "e",
        function()
          actions.select_entry()
          vim.schedule(function() vim.cmd("wincmd b") end)
        end,
        { desc = "Open + edit the selected file" },
      },
    },
  },
})

-- Muted diff colours. Dark backgrounds only -- the foreground is left to
-- syntax/treesitter so text stays high-contrast and readable (bright bg + dark
-- fg was the low-contrast problem). Re-applied on ColorScheme so it survives a
-- colorscheme reload.
local function diff_colors()
  vim.api.nvim_set_hl(0, "DiffAdd",    { bg = "#1e3328" }) -- added line: muted green
  vim.api.nvim_set_hl(0, "DiffChange", { bg = "#25303f" }) -- changed line: subtle blue-grey
  vim.api.nvim_set_hl(0, "DiffDelete", { bg = "#382630" }) -- removed line: muted red
  vim.api.nvim_set_hl(0, "DiffText",   { bg = "#2c4d3a" }) -- changed text within a line
end
vim.api.nvim_create_autocmd("ColorScheme", { callback = diff_colors })
diff_colors()

-- Review keymaps
local opts = { silent = true, noremap = true }
vim.keymap.set("n", "<leader>dd", "<cmd>DiffviewOpen<cr>", opts)            -- uncommitted changes vs HEAD
vim.keymap.set("n", "<leader>dm", "<cmd>DiffviewOpen main..HEAD<cr>", opts) -- this branch's commits vs main
vim.keymap.set("n", "<leader>dh", "<cmd>DiffviewFileHistory<cr>", opts)     -- repo commit history
vim.keymap.set("n", "<leader>dq", "<cmd>DiffviewClose<cr>", opts)           -- close the review tab

-- <leader>dP: quick-ship whatever is STAGED -- commit (whatthecommit message,
-- like the gitCba shell alias) + push, async. Stage files in diffview with s/S,
-- then hit this. Does NOT `git add`, so it commits exactly what you staged.
local function quick_push_staged()
  local function note(msg, level)
    vim.schedule(function()
      vim.notify(msg, level or vim.log.levels.INFO, { title = "git" })
    end)
  end

  -- Anything staged? (`--quiet` exits 0 when there are NO staged changes.)
  if vim.system({ "git", "diff", "--cached", "--quiet" }):wait().code == 0 then
    note("Nothing staged - stage files first (diffview: s / S).", vim.log.levels.WARN)
    return
  end

  vim.system({ "curl", "-s", "--max-time", "5", "https://whatthecommit.com/" }, { text = true }, function(curl_res)
    local msg = (curl_res.stdout or ""):match("<p>%s*(.-)%s*<")
    if not msg or msg == "" then msg = "lazy push " .. os.date("%Y-%m-%d %H:%M") end

    vim.system({ "git", "commit", "-m", msg }, { text = true }, function(commit_res)
      if commit_res.code ~= 0 then
        note("commit failed:\n" .. (commit_res.stderr or commit_res.stdout or ""), vim.log.levels.ERROR)
        return
      end
      note('committed "' .. msg .. '" - pushing...')

      vim.system({ "git", "push" }, { text = true }, function(push_res)
        if push_res.code == 0 then
          note("pushed " .. msg)
        else
          note("push failed:\n" .. (push_res.stderr or ""), vim.log.levels.ERROR)
        end
      end)
    end)
  end)
end

vim.keymap.set("n", "<leader>dP", quick_push_staged,
  { silent = true, noremap = true, desc = "Commit staged + push (lazy)" })
