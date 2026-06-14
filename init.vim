set runtimepath^=~/.vim runtimepath+=~/.vim/after
let &packpath = &runtimepath

" Old vim configs
source ~/dotfiles/globals.vim


lua << EOF

-- Plugins are managed by Neovim's native plugin manager `vim.pack` (Neovim 0.12+).
-- This replaces Vundle. Plugins live under ~/.local/share/nvim/site/pack/core/opt/
-- and are pinned via ~/.config/nvim/nvim-pack-lock.json.
--   :lua vim.pack.update()            update all plugins (shows a confirm buffer)
--   :lua vim.pack.update({ "name" })  update one plugin
--   :lua print(vim.inspect(vim.pack.get()))  list managed plugins
vim.pack.add({
  -- Utilities
  { src = 'https://github.com/kkoomen/vim-doge' },        -- Auto doc comment (run :call doge#install() once)
  { src = 'https://github.com/preservim/nerdcommenter' }, -- Auto comment
  { src = 'https://github.com/windwp/nvim-autopairs' },
  { src = 'https://github.com/nvim-telescope/telescope.nvim' },
  { src = 'https://github.com/nvim-lua/plenary.nvim' },

  -- Completion: blink.cmp replaces nvim-cmp + all cmp-* sources + vsnip.
  -- Pinned to the stable v1 line so the prebuilt fuzzy binary is fetched.
  { src = 'https://github.com/saghen/blink.cmp', version = vim.version.range('1') },
  { src = 'https://github.com/xzbdmw/colorful-menu.nvim' },

  -- LSP (config registry; servers enabled via native vim.lsp.enable in lua/mlsp.lua)
  { src = 'https://github.com/neovim/nvim-lspconfig' },

  { src = 'https://gitlab.com/HiPhish/rainbow-delimiters.nvim' },

  -- Code forces
  { src = 'https://github.com/gabrielsimoes/cfparser.vim' },

  -- Icons (nvim-web-devicons replaces the old vimscript vim-devicons)
  { src = 'https://github.com/nvim-tree/nvim-web-devicons' },

  -- Navigation
  { src = 'https://github.com/ms-jpq/chadtree' },         -- run :CHADdeps once to build the python venv

  -- Language Specific
  { src = 'https://github.com/rust-lang/rust.vim' },
  { src = 'https://github.com/evanleck/vim-svelte' },
  { src = 'https://github.com/uarun/vim-protobuf' },
  { src = 'https://github.com/cakebaker/scss-syntax.vim' },
  { src = 'https://github.com/chr4/nginx.vim' },

  -- Markdown viewer
  { src = 'https://github.com/OXY2DEV/markview.nvim' },

  -- Git
  { src = 'https://github.com/tpope/vim-fugitive' },

  -- Treesitter: `main` branch (the 0.12 rewrite). The old `master` branch is
  -- frozen and incompatible with Neovim 0.12. `main` provides parsers + queries;
  -- highlighting is driven by Neovim's native vim.treesitter (see lua/mtreesitter.lua).
  -- Needs the tree-sitter CLI (~/.local/bin/tree-sitter) + a C compiler to build parsers.
  { src = 'https://github.com/nvim-treesitter/nvim-treesitter', version = 'main' },

  -- UI
  { src = 'https://github.com/MunifTanjim/nui.nvim' },
  { src = 'https://github.com/rcarriga/nvim-notify' },
  { src = 'https://github.com/folke/noice.nvim' },

  { src = 'https://github.com/stevearc/aerial.nvim' },
  { src = 'https://github.com/stevearc/stickybuf.nvim' },
}, { confirm = false })


vim.g.chadtree_settings = {
  -- other root-level settings if any
  theme = {
    text_colour_set = "solarized_universal"
  },
  options = {
    -- other options if any
    version_control = {
      enable = false
    }
  },
  ignore = {
    name_exact = {".git"}  -- Also ignore .git folders
  }
}


require("stickybuf").setup()

require('nvim-autopairs').setup({
    map_cr = false
})

require("mtelescope") -- My Telescope plugin
require("mlsp") -- My LSP config (completion + native LSP servers)
require("mtreesitter")
require("maerial")

vim.diagnostic.config({  -- https://neovim.io/doc/user/diagnostic.html
    virtual_text = true,
    signs = false,
    underline = true,
})



vim.keymap.set('n', '<C-j>', function() vim.diagnostic.jump({ count = 1, float = true }) end, {silent = true, noremap = true})
vim.keymap.set('n', '<C-k>', function() vim.diagnostic.jump({ count = -1, float = true }) end, {silent = true, noremap = true})
vim.keymap.set('n', '<C-e>', '<cmd>CHADopen<cr>', {silent = true, noremap = true})
vim.keymap.set('n', '<leader>j', '<cmd>cnext<cr>', {silent = true, noremap = true})
vim.keymap.set('n', '<leader>k', '<cmd>cprevious<cr>', {silent = true, noremap = true})

require("noice").setup({
  messages = {
    -- NOTE: If you enable messages, then the cmdline is enabled automatically.
    -- This is a current Neovim limitation.
    enabled = true, -- enables the Noice messages UI
    view = "notify", -- default view for messages
    view_error = "notify", -- view for errors
    view_warn = "notify", -- view for warnings
    view_history = "messages", -- view for :messages
    view_search = "virtualtext", -- view for search count messages. Set to `false` to disable
  },

  lsp = {
    -- override markdown rendering so that completion docs use **Treesitter**
    override = {
      ["vim.lsp.util.convert_input_to_markdown_lines"] = true,
      ["vim.lsp.util.stylize_markdown"] = true,
    },
    -- basedpyright fires $/progress on every edit; don't surface that as notifications.
    progress = {
      enabled = false,
    },
    message = {
      -- Messages shown by lsp servers
      enabled = true,
      view = "hover",
    },
  },
  popupmenu = {
    -- blink.cmp owns command-line completion now, so noice's own popupmenu is
    -- disabled to avoid two competing completion menus.
    enabled = false,
  },

  -- you can enable a preset for easier configuration
  presets = {
    bottom_search = true, -- use a classic bottom cmdline for search
    command_palette = true, -- position the cmdline and popupmenu together
    long_message_to_split = true, -- long messages will be sent to a split
    inc_rename = false, -- enables an input dialog for inc-rename.nvim
    lsp_doc_border = false, -- add a border to hover docs and signature help
  },
})

local notify = require("notify")
notify.setup({
        render = "wrapped-compact",
        stages = "no_animation", -- <==== This is the workaround
      })
vim.notify = notify

for _, method in ipairs({ 'textDocument/diagnostic', 'workspace/diagnostic' }) do
    local default_diagnostic_handler = vim.lsp.handlers[method]
    vim.lsp.handlers[method] = function(err, result, context, config)
        if err ~= nil and err.code == -32802 then
            return
        end
        return default_diagnostic_handler(err, result, context, config)
    end
end


EOF
