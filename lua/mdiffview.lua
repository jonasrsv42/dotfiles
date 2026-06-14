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
