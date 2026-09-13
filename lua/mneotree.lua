-- neo-tree file explorer.
-- Replaces chadtree. Key difference: "reveal current file" and "follow the
-- current file" are separate here. Following is OFF so the tree never yanks the
-- cursor back while you browse; `:Neotree reveal` (bound to <C-e>/<leader>e in
-- init.vim) locates the current file on demand.

require("neo-tree").setup({
  close_if_last_window = false,    -- :q on the last file window leaves neo-tree open (chadtree-like)
  enable_git_status = false,       -- mirrors chadtree version_control.enable = false
  enable_diagnostics = false,
  default_component_configs = {
    indent = { with_markers = false },
    git_status = { symbols = {} },
  },
  window = {
    position = "left",
    width = 35,
    mappings = {
      ["<space>"] = "none",         -- <space> is <leader>; don't let neo-tree eat it
      ["l"] = "open",
      ["h"] = "close_node",
      ["s"] = "open_split",
      ["v"] = "open_vsplit",
      ["<C-e>"] = "close_window",   -- same key closes the tree from inside it
      -- Jump the tree cursor to the file open in the editor (chadtree's J).
      -- Copy the absolute path of the node under the cursor to the system
      -- clipboard (what chadtree's Y did). gy copies just the file name.
      ["Y"] = function(state)
        local path = state.tree:get_node():get_id()
        vim.fn.setreg("+", path)
        vim.notify("Copied: " .. path)
      end,
      ["gy"] = function(state)
        local name = state.tree:get_node().name
        vim.fn.setreg("+", name)
        vim.notify("Copied: " .. name)
      end,
      ["J"] = function()
        require("neo-tree.command").execute({ action = "focus", source = "filesystem", reveal = true })
      end,
    },
  },
  filesystem = {
    follow_current_file = { enabled = false },
    use_libuv_file_watcher = true,  -- refresh on disk changes without polling
    filtered_items = {
      visible = false,
      hide_dotfiles = false,
      hide_gitignored = false,
      hide_by_name = { ".git" },    -- mirrors chadtree ignore.name_exact
    },
  },
})
