set runtimepath^=~/.vim runtimepath+=~/.vim/after
let &packpath = &runtimepath

set rtp+=~/.vim/bundle/Vundle.vim


call vundle#begin()
" alternatively, pass a path where Vundle should install plugins

" let Vundle manage Vundle, required
Plugin 'VundleVim/Vundle.vim'

" Utilities
Plugin 'kkoomen/vim-doge' " Auto doc comment
Plugin 'scrooloose/nerdcommenter' "Auto comment


Plugin 'windwp/nvim-autopairs'
Plugin 'nvim-telescope/telescope.nvim'
Plugin 'nvim-lua/plenary.nvim'

" Neovim LSP
Plugin 'onsails/lspkind.nvim'
Plugin 'hrsh7th/nvim-cmp'
Plugin 'hrsh7th/cmp-buffer'
Plugin 'hrsh7th/cmp-path'
Plugin 'hrsh7th/cmp-cmdline'
Plugin 'hrsh7th/cmp-nvim-lsp'
Plugin 'hrsh7th/cmp-nvim-lsp-signature-help'
Plugin 'neovim/nvim-lspconfig'
Plugin 'nvimtools/none-ls.nvim'
Plugin 'xzbdmw/colorful-menu.nvim'

Plugin 'https://gitlab.com/HiPhish/rainbow-delimiters.nvim'

" Code forces
Plugin 'gabrielsimoes/cfparser.vim'

" Niceities
Plugin 'ryanoasis/vim-devicons'

" Navigation
Plugin 'ms-jpq/chadtree'

" Language Specific
Plugin 'rust-lang/rust.vim'
Plugin 'evanleck/vim-svelte'
Plugin 'uarun/vim-protobuf'
Plugin 'cakebaker/scss-syntax.vim'
Plugin 'chr4/nginx.vim'

Plugin 'scottmckendry/cyberdream.nvim'

" Git
Plugin 'tpope/vim-fugitive'
Plugin 'nvim-treesitter/nvim-treesitter'

"UI 
Plugin 'MunifTanjim/nui.nvim'
Plugin 'rcarriga/nvim-notify'
Plugin 'folke/noice.nvim'

Plugin 'stevearc/aerial.nvim'
Plugin 'stevearc/stickybuf.nvim'

" Markdown viewer
Plugin 'OXY2DEV/markview.nvim'

call vundle#end()            " required

" Old vim configs
source ~/dotfiles/globals.vim


lua << EOF

require("stickybuf").setup()

require('nvim-autopairs').setup({
    map_cr = false
})

require("mtelescope") -- My Telescope plugin
require("mlsp") -- My LSP config
require("none_ls") -- Additional LSP integrations.
require("mtreesitter") 
require("maerial") 

vim.diagnostic.config({  -- https://neovim.io/doc/user/diagnostic.html
    virtual_text = true,
    signs = false,
    underline = true,
})



vim.keymap.set('n', '<C-j>', vim.diagnostic.goto_next, {silent = True, noremap = True})
vim.keymap.set('n', '<C-k>', vim.diagnostic.goto_prev, {silent = True, noremap = True})
vim.keymap.set('n', '<C-e>', '<cmd>CHADopen<cr>', {silent = True, noremap = True})

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
    -- override markdown rendering so that **cmp** and other plugins use **Treesitter**
    override = {
      ["vim.lsp.util.convert_input_to_markdown_lines"] = true,
      ["vim.lsp.util.stylize_markdown"] = true,
      ["cmp.entry.get_documentation"] = true, -- requires hrsh7th/nvim-cmp
    },
    message = {
      -- Messages shown by lsp servers
      enabled = true,
      view = "hover",
    },
  },
  popupmenu = {
    enabled = true, -- enables the Noice popupmenu UI
    ---@type 'nui'|'cmp'
    backend = "cmp", -- backend to use to show regular cmdline completions
    ---@type NoicePopupmenuItemKind|false
    -- Icons for completion item kinds (see defaults at noice.config.icons.kinds)
    kind_icons = {}, -- set to `false` to disable icons
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

