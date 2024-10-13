let g:chadtree_settings = { "theme.text_colour_set": "solarized_universal" }
let g:python3_host_prog = '/usr/bin/python3'
set completeopt=menuone,noselect


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

" Default Indentation
set tabstop=2 "not sure"
set shiftwidth=2  "tab width"
set expandtab "Convert tab to spaces"
set ai "Keeps indentation from last line"
set nu

" window splits are automatically on right now"
set splitright
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

nnoremap tb :TagbarToggle<CR>

au FileType netrw setl bufhidden=delete

au FileType python set tabstop=4
au FileType python set shiftwidth=4  
