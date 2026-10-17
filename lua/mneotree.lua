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
    -- bloaty: a dim line count and bar for each file/dir's share of its directory's impl
    -- lines, `•` where it found bloat, `●` where it smells
    -- (lua/mbloaty/tree.lua). The renderers are neo-tree v3's defaults plus it;
    -- it gives way first (zindex 5) when names need the width.
    components = {
      -- (config, node, state, remaining width)
      bloaty = function(...)
        return require("mbloaty.tree").component(...)
      end,
    },
    window = {
      mappings = {
        -- bloaty: explain the line count / smell of the node under the cursor.
        ["B"] = function(state)
          require("mbloaty.tree").explain(state)
        end,
      },
    },
    renderers = {
      directory = {
        { "indent" },
        { "icon" },
        { "current_filter" },
        {
          "container",
          content = {
            { "name", zindex = 10 },
            { "symlink_target", zindex = 10, highlight = "NeoTreeSymbolicLinkTarget" },
            { "clipboard", zindex = 10 },
            { "diagnostics", errors_only = true, zindex = 20, align = "right", hide_when_expanded = true },
            { "git_status", zindex = 10, align = "right", hide_when_expanded = true },
            { "bloaty", zindex = 5, align = "right" },
            { "file_size", zindex = 10, align = "right" },
            { "type", zindex = 10, align = "right" },
            { "last_modified", zindex = 10, align = "right" },
            { "created", zindex = 10, align = "right" },
          },
        },
      },
      file = {
        { "indent" },
        { "icon" },
        {
          "container",
          content = {
            { "name", zindex = 10 },
            { "symlink_target", zindex = 10, highlight = "NeoTreeSymbolicLinkTarget" },
            { "clipboard", zindex = 10 },
            { "bufnr", zindex = 10 },
            { "modified", zindex = 20, align = "right" },
            { "diagnostics", zindex = 20, align = "right" },
            { "git_status", zindex = 10, align = "right" },
            { "bloaty", zindex = 5, align = "right" },
            { "file_size", zindex = 10, align = "right" },
            { "type", zindex = 10, align = "right" },
            { "last_modified", zindex = 10, align = "right" },
            { "created", zindex = 10, align = "right" },
          },
        },
      },
    },
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
