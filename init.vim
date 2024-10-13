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

Plugin 'simrat39/rust-tools.nvim'

Plugin 'https://gitlab.com/HiPhish/rainbow-delimiters.nvim'

" Code forces
Plugin 'gabrielsimoes/cfparser.vim'

" Niceities
Plugin 'ryanoasis/vim-devicons'

" Navigation
Plugin 'majutsushi/tagbar'
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
call vundle#end()            " required

" Old vim configs
source ~/dotfiles/globals.vim


lua << EOF

require('nvim-autopairs').setup({
    map_cr = false
})

require("mtelescope") -- My Telescope plugin
require("mlsp") -- My LSP config
require("none_ls") -- Additional LSP integrations.

vim.diagnostic.config({  -- https://neovim.io/doc/user/diagnostic.html
    virtual_text = true,
    signs = false,
    underline = true,
})


vim.keymap.set('n', '<C-j>', vim.diagnostic.goto_next, {silent = True, noremap = True})
vim.keymap.set('n', '<C-k>', vim.diagnostic.goto_prev, {silent = True, noremap = True})

EOF
