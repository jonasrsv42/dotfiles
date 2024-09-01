set rtp+=~/.vim/bundle/Vundle.vim


call vundle#begin()
" alternatively, pass a path where Vundle should install plugins

" let Vundle manage Vundle, required
Plugin 'VundleVim/Vundle.vim'

" Utilities
Plugin 'kkoomen/vim-doge' " Auto doc comment
Plugin 'scrooloose/nerdcommenter' "Auto comment

Plugin 'onsails/lspkind.nvim'

Plugin 'windwp/nvim-autopairs'
Plugin 'nvim-telescope/telescope.nvim'
Plugin 'nvim-lua/plenary.nvim'

" Neovim LSP
Plugin 'hrsh7th/nvim-cmp'
Plugin 'hrsh7th/cmp-buffer'
Plugin 'hrsh7th/cmp-path'
Plugin 'hrsh7th/cmp-cmdline'
Plugin 'hrsh7th/cmp-nvim-lsp'
Plugin 'hrsh7th/cmp-nvim-lsp-signature-help'
Plugin 'neovim/nvim-lspconfig'

Plugin 'simrat39/rust-tools.nvim'

Plugin 'hrsh7th/vim-vsnip'
Plugin 'hrsh7th/vim-vsnip-integ'

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


let g:chadtree_settings = { "theme.text_colour_set": "solarized_universal" }
let g:python3_host_prog = '/usr/bin/python3'
set completeopt=menuone,noselect


lua << EOF

-- vim.lsp.set_log_level("debug")
require('nvim-autopairs').setup({
    map_cr = false
})


require('telescope').setup{
defaults = {
  vimgrep_arguments = {
    'rg',
    '--color=never',
    '--no-heading',
    '--with-filename',
    '--line-number',
    '--column',
    '--smart-case'
    },
    prompt_prefix = "> ",
    selection_caret = "> ",
    entry_prefix = "  ",
    initial_mode = "insert",
    selection_strategy = "reset",
    sorting_strategy = "descending",
    layout_strategy = "horizontal",
    layout_config = {
      horizontal = {
        mirror = false,
        },
        vertical = {
          mirror = false,
          },
        },
        file_sorter =  require'telescope.sorters'.get_fuzzy_file,
        file_ignore_patterns = {},
        generic_sorter =  require'telescope.sorters'.get_generic_fuzzy_sorter,
        winblend = 0,
        border = {},
        borderchars = { '─', '│', '─', '│', '╭', '╮', '╯', '╰' },
        color_devicons = true,
        use_less = true,
        path_display = {},
        set_env = { ['COLORTERM'] = 'truecolor' }, -- default = nil,
        file_previewer = require'telescope.previewers'.vim_buffer_cat.new,
        grep_previewer = require'telescope.previewers'.vim_buffer_vimgrep.new,
        qflist_previewer = require'telescope.previewers'.vim_buffer_qflist.new,

        -- Developer configurations: Not meant for general override
        buffer_previewer_maker = require'telescope.previewers'.buffer_previewer_maker
}
}

-- Set up nvim-cmp.
local cmp = require'cmp'
local lspkind = require('lspkind')

cmp.setup({
experimental = {
    ghost_text = true,
},
snippet = {
  -- REQUIRED - you must specify a snippet engine
  expand = function(args)
  vim.fn["vsnip#anonymous"](args.body) -- For `vsnip` users.
  end,
  },
  window = {
    completion = cmp.config.window.bordered(),
    documentation = cmp.config.window.bordered(),
  },
  mapping = cmp.mapping.preset.insert({
    ['<C-e>'] = cmp.mapping.abort(),
    ['<CR>'] = cmp.mapping.confirm({ select = false }), -- Accept currently selected item. Set `select` to `false` to only confirm explicitly selected items.
  }),
  sources = cmp.config.sources({
      { name = 'nvim_lsp' },
      { name = 'nvim_lsp_signature_help' },
      { name = 'vsnip' }, -- For vsnip users.
    }, {
      { name = 'buffer' },
  }),
  formatting = {
    format = lspkind.cmp_format({
      mode = 'symbol', -- show only symbol annotations
      maxwidth = 50, -- prevent the popup from showing more than provided characters (e.g 50 will not show more than 50 characters)
                     -- can also be a function to dynamically calculate max width such as
                     -- maxwidth = function() return math.floor(0.45 * vim.o.columns) end,
      ellipsis_char = '...', -- when popup menu exceed maxwidth, the truncated part would show ellipsis_char instead (must define maxwidth first)
      show_labelDetails = true, -- show labelDetails in menu. Disabled by default

      -- The function below will be called before any actual modifications from lspkind
      -- so that you can provide more controls on popup customization. (See [#30](https://github.com/onsails/lspkind-nvim/pull/30))
      before = function (entry, vim_item)
        return vim_item
      end
    })
  }
})


-- Set configuration for specific filetype.
cmp.setup.filetype('gitcommit', {
  sources = cmp.config.sources({
  { name = 'git' }, -- You can specify the `git` source if [you were installed it](https://github.com/petertriho/cmp-git).
  }, {
    { name = 'buffer' },
  })
})

-- Use buffer source for `/` and `?` (if you enabled `native_menu`, this won't work anymore).
cmp.setup.cmdline({ '/', '?' }, {
  mapping = cmp.mapping.preset.cmdline(),
  sources = {
    { name = 'buffer' }
  }
})

-- Use cmdline & path source for ':' (if you enabled `native_menu`, this won't work anymore).
cmp.setup.cmdline(':', {
  mapping = cmp.mapping.preset.cmdline(),
  sources = cmp.config.sources({
  { name = 'path' }
  }, {
    { name = 'cmdline' }
  })
})


-- Set up lspconfig.
local capabilities = require('cmp_nvim_lsp').default_capabilities()

local on_attach = function(client, buf)

-- 

end




-- Setup rust_analyzer via rust-tools.nvim
require'lspconfig'.rust_analyzer.setup{
  server = {
    capabilities = capabilities,
    on_attach = on_attach,
  }
}

require'lspconfig'.pylsp.setup{
server = {
  capabilities = capabilities,
  on_attach = on_attach,
  },
  settings = {
    pylsp = {
     -- formatter options
      black = { enabled = true },
      autopep8 = { enabled = false },
      yapf = { enabled = false },
      -- linter options
      pylint = { enabled = true, executable = "pylint" },
      pyflakes = { enabled = false },
      pycodestyle = { enabled = false },
      -- type checker
      pylsp_mypy = { enabled = true },
      -- auto-completion options
      jedi_completion = { fuzzy = true },
      -- import sorting
      pyls_isort = { enabled = true },
      plugins = {
        pycodestyle = {
          ignore = {'W391'},
          maxLineLength = 100
        }
        }
      }
    }
  }

require'lspconfig'.clangd.setup {
  server = {
    capabilities = capabilities,
    on_attach = on_attach
  }
}


EOF

nnoremap <silent> <leader>F <cmd>lua vim.lsp.buf.format()<CR>
nnoremap <silent> gd <cmd>lua vim.lsp.buf.definition()<CR>
nnoremap <silent> K  <cmd>lua vim.lsp.buf.hover()<CR>
nnoremap <silent> gr <cmd>lua vim.lsp.buf.references()<CR>
nnoremap <silent> <C-space> <cmd>lua vim.lsp.buf.code_action()<CR>
nnoremap <silent> <C-s> <cmd>lua vim.lsp.buf.signature_help()<CR>

nnoremap <C-f> :Telescope find_files<CR>
nnoremap <leader>f :Telescope grep_string<CR>

nnoremap <C-t> :Telescope<CR>


imap <expr> <Tab>   vsnip#jumpable(1)   ? '<Plug>(vsnip-jump-next)'      : '<Tab>'
smap <expr> <Tab>   vsnip#jumpable(1)   ? '<Plug>(vsnip-jump-next)'      : '<Tab>'
imap <expr> <S-Tab> vsnip#jumpable(-1)  ? '<Plug>(vsnip-jump-prev)'      : '<S-Tab>'
smap <expr> <S-Tab> vsnip#jumpable(-1)  ? '<Plug>(vsnip-jump-prev)'      : '<S-Tab>'


set backupdir=/home/jonasrsv/.backups
set directory=/home/jonasrsv/.backups


let g:disable_vim_auto_close_plugin = 1

set laststatus=0
set splitbelow

set encoding=utf-8
set guifont=FiraCode\ Nerd\ Font\ Mono\ 12
set clipboard+=unnamed,unnamedplus
set nocompatible "Not sure what it does but people claim its useful"
set path=~/ "Path to root"
set mouse=a "enable mouse"

"Switching Buffers without saving"
set hidden

"For Regexes"
set magic

"Fix Searching"
set hlsearch "Highligh search hits"
set incsearch "Not sure"

set sh=/bin/zsh "Shell to use"

set ef=e.err " not sure"
set title "Enable Title on window"


set wildmenu "Autocompletion in commandline"
set wildmode=longest:full,full "How to autocomplete"
set wildignore+=*/target/*,*/.git/*,*/node_modules/* "What autocomplete ignores"
set tagstack "Enables Stack for tags"
set autoread "Not Sure But useful apparently"

set showmatch

"Default Indentation"
set tabstop=2 "not sure"
set shiftwidth=2  "tab width"
set expandtab "Convert tab to spaces"
set ai "Keeps indentation from last line"
set nu

" window splits are automatically on right now"
set splitright

autocmd BufReadPre,BufNewFile * let b:did_ftplugin = 1

set termguicolors
colo mycolo


let &t_8f = "\<Esc>[38:2:%lu:%lu:%lum"
let &t_8b = "\<Esc>[48:2:%lu:%lu:%lum"

syntax on
filetype plugin indent on

if has("nvim")
  nnoremap <leader><C-space> :vertical sbp<CR>`"zz
else
  map <leader><C-@> :vertical sbp<CR>`"zz
endif


nnoremap <leader>p <C-^>`"zz
nnoremap n nzz
nnoremap N Nzz

nnoremap j gj
nnoremap k gk

nnoremap <C-w> <C-w>w
nnoremap <C-e> :CHADopen<CR>

map <space> <leader>
map <C-c> :bd<CR>
map ö 7j
map ä 7k
map Y 0y$
map Z zz

nnoremap L Lzz
nnoremap H Hzz

"For syncing clipboard on unix systems with xclip"

map <C-y> yy \| :call system("xclip -selection clipboard -in", @0)<CR>
map <C-p> :r !xclip -selection clipboard -o<CR>


"stty -ixon IS NEEDED FOR C-s binding put in *rc


"highlight StatusLine guibg=#00000 guifg=#b8ff73
"highlight QuickFixLine term=bold,underline cterm=bold,underline gui=bold,underline
"highlight Folded guibg=#3a3c3f guifg=#c0c4ce
"hi FoldColumn guifg=#00000 guibg=#00000
"hi SignColumn guifg=#00000 guibg=#00000


nnoremap tb :TagbarToggle<CR>


"nnoremap  gg=G<C-o><C-o>zz"
nnoremap <C-j> :lua vim.diagnostic.goto_next()<CR>zz
nnoremap <C-k> :lua vim.diagnostic.goto_prev()<CR>zz

command! Make execute "make " . expand("%") . " | redraw! | vertical cope | vertical resize 100 | wincmd p"

nnoremap <C-m> :Make<CR>


au BufRead,BufNewFile *.journal set filetype=journal

au FileType netrw setl bufhidden=delete
au FileType go setlocal equalprg=gofmt
au FileType javascript setlocal equalprg=js-beautify\ --stdin
au FileType haskell setlocal equalprg=stylish-haskell
au FileType json setlocal equalprg=js-beautify

au FileType python setlocal makeprg=python3\ %
au FileType python setlocal equalprg=cookicookie
au FileType python compiler python
au FileType python nnoremap <F10> :silent exec "!python3 %"<CR>

au FileType python set tabstop=4
au FileType python set shiftwidth=4  

au FileType html nnoremap <buffer> <leader><F10> :!xdg-open %<CR>

au FileType cpp setlocal equalprg=clang-format

au FileType rust setlocal makeprg=cargo\ run\ %




func RenderTex()
  silent! call system("latexmk -pdf ")
endfun

func WritingMode()
  setlocal statusline=\
  setlocal nonu
  setlocal nornu
  setlocal colorcolumn=
  setlocal laststatus=0
  hi FoldColumn guifg=#00000 guibg=#00000
  hi SignColumn guifg=#00000 guibg=#00000
  set wm=20
  set spell
endfun

augroup latex
  autocmd!
  au BufWritePost *.tex call RenderTex()
  au FileType tex call WritingMode()
augroup END

augroup remember_folds
  autocmd!
  autocmd BufWinLeave *.py mkview
  autocmd BufWinLeave *.go mkview
  autocmd BufWinEnter *.py silent! loadview
  autocmd BufWinEnter *.go silent! loadview
augroup END


