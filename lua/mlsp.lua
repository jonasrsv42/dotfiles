-- Completion is handled by blink.cmp, which replaces nvim-cmp and all of the
-- cmp-* source plugins (buffer/path/cmdline/nvim_lsp/signature_help) plus vsnip.
-- LSP, path, snippet and buffer sources, signature help and command-line
-- completion are all built in.
local colorful = require("colorful-menu")

require("blink.cmp").setup({
  keymap = {
    preset = "default",                 -- <C-n>/<C-p> or <Up>/<Down> to select, <C-y> to accept
    ["<CR>"] = { "accept", "fallback" }, -- Enter accepts the selected item (no auto-preselect, see below)
    ["<C-e>"] = { "hide", "fallback" },
  },

  appearance = {
    nerd_font_variant = "mono",         -- built-in kind icons (replaces lspkind)
  },

  -- Built-in signature help (replaces cmp-nvim-lsp-signature-help)
  signature = { enabled = true },

  sources = {
    default = { "lsp", "path", "snippets", "buffer" },
  },

  fuzzy = { implementation = "prefer_rust_with_warning" },

  completion = {
    -- Don't preselect/auto-insert; Enter only confirms an explicitly selected
    -- item. Matches the old nvim-cmp `confirm({ select = false })` behaviour.
    list = { selection = { preselect = false, auto_insert = true } },

    ghost_text = { enabled = true },

    documentation = {
      auto_show = true,
      window = { border = "single" },
    },

    menu = {
      border = "single",
      draw = {
        -- colorful-menu.nvim owns label highlighting; do NOT also enable blink's
        -- built-in `treesitter = { "lsp" }` here -- running both makes blink
        -- re-parse the label and crash with "treesitter.lua: method 'range' (nil)".
        columns = { { "kind_icon" }, { "label", gap = 1 } },
        components = {
          label = {
            text = function(ctx) return colorful.blink_components_text(ctx) end,
            highlight = function(ctx) return colorful.blink_components_highlight(ctx) end,
          },
        },
      },
    },
  },

  cmdline = {
    keymap = {
      preset = "cmdline",
      -- <Tab> accepts the current suggestion instead of cycling to the next.
      -- (The cmdline preset keeps <C-n>/<C-p> and arrows for cycling.)
      ["<Tab>"] = { "select_and_accept", "fallback" },
    },
    completion = { menu = { auto_show = true } },
  },
})


-- ---------------------------------------------------------------------------
-- LSP: native vim.lsp.config / vim.lsp.enable (Neovim 0.11+).
-- nvim-lspconfig is on the runtimepath purely to supply the base server
-- definitions (cmd, filetypes, root markers) under vim.lsp.config.
-- ---------------------------------------------------------------------------

-- Apply blink.cmp's completion capabilities to every server.
vim.lsp.config("*", {
  capabilities = require("blink.cmp").get_lsp_capabilities(),
})

vim.lsp.config("rust_analyzer", {
  settings = {
    ["rust-analyzer"] = {
      cargo = { allFeatures = false },
    },
  },
})

-- basedpyright: types / completion / hover.  ruff: lint, format, import sort.
-- Both are installed ONCE as standalone tools via `uv tool install` (shims on
-- ~/.local/bin) -- NOT per project venv. They are venv-aware: they analyze
-- whichever environment belongs to the project being edited.
--
-- Picking the project's interpreter:
--   * If a virtualenv is active ($VIRTUAL_ENV), use its interpreter.
--   * Otherwise leave pythonPath unset and let basedpyright auto-detect a
--     .venv / venv in the project root (or a pyrightconfig.json / pyproject.toml).
local active_venv = vim.env.VIRTUAL_ENV
local use_venv = active_venv ~= nil and active_venv ~= ""

vim.lsp.config("basedpyright", {
  settings = {
    basedpyright = {
      analysis = {
        typeCheckingMode = "standard",
        diagnosticMode = "openFilesOnly",
      },
    },
    python = use_venv and { pythonPath = active_venv .. "/bin/python" } or nil,
  },
})

vim.lsp.config("ruff", {
  -- Let basedpyright own hover; ruff just lints/formats.
  on_attach = function(client, _)
    client.server_capabilities.hoverProvider = false
  end,
})

vim.lsp.enable({
  "rust_analyzer",
  "vtsls",
  "basedpyright",
  "ruff",
  "protols",
})


vim.keymap.set('n', '<leader>F', vim.lsp.buf.format, {silent = true, noremap = true})
vim.keymap.set('n', 'gd', vim.lsp.buf.definition, {silent = true, noremap = true})
vim.keymap.set('n', 'K', vim.lsp.buf.hover, {silent = true, noremap = true})
vim.keymap.set('n', 'gr', vim.lsp.buf.references, {silent = true, noremap = true})
vim.keymap.set('n', '<C-space>', vim.lsp.buf.code_action, {silent = true, noremap = true})
vim.keymap.set('n', '<C-s>', vim.lsp.buf.signature_help, {silent = true, noremap = true})
