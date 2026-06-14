
fg_black="$(tput setaf 0)"
fg_red="$(tput setaf 1)"
fg_green="$(tput setaf 2)"
fg_yellow="$(tput setaf 3)"
fg_blue="$(tput setaf 4)"
fg_magenta="$(tput setaf 5)"
fg_cyan="$(tput setaf 6)"
fg_white="$(tput setaf 7)"
reset="$(tput sgr0)"

echo "${fg_red}$ This setup file is incomplete ${reset}"

if ! command -v git 
then 
  echo "${fg_red}git not found Please install it${fg_reset}"
  echo "Try:"
  echo "       sudo apt-get install git"
  exit 1
fi

echo "${fg_green}$ Setting up for zsh ${reset}"

if ! command -v zsh 
then 
  echo "${fg_red}zsh not found Please install it${fg_reset}"
  echo "Try:"
  echo "       sudo apt-get install zsh"
  exit 1
fi

if ! command -v kitty
then
  echo "${fg_red}Kitty not found Please install it${fg_reset}"
  echo "See:"
  echo "      https://sw.kovidgoyal.net/kitty/binary/"
  exit 1
fi

KITTY_CONFIG=$HOME/.config/kitty/kitty.conf
if [ ! -f $KITTY_CONFIG ]; then
  echo "${fg_green} Linking $HOME/dotfiles/kitty.conf -> $KITTY_CONFIG ${reset}"
  ln -s $HOME/dotfiles/kitty.conf $KITTY_CONFIG 
else 
  echo "${fg_magenta} $KITTY_CONFIG already exists - skipping... ${reset}"
fi

ZSHRC_PATH=$HOME/.zshrc
if [ ! -f $ZSHRC_PATH ]; then
  echo "${fg_green} Linking $HOME/dotfiles/.zshrc -> $ZSHRC_PATH ${reset}"
  ln -s $HOME/dotfiles/.zshrc $ZSHRC_PATH
else
  echo "${fg_magenta} $ZSHRC_PATH already exists - skipping... ${reset}"
fi

# Pure https://github.com/sindresorhus/pure
ZSH_PROMPT_ASYNC_PATH=/usr/local/share/zsh/site-functions/async
if [ ! -f $ZSH_PROMPT_ASYNC_PATH ]; then
  echo "${fg_green} Linking $HOME/dotfiles/zsh-scripts/pure_async.zsh -> $ZSH_PROMPT_ASYNC_PATH ${reset}"
  sudo ln -s $HOME/dotfiles/zsh-scripts/pure_async.zsh $ZSH_PROMPT_ASYNC_PATH
else
  echo "${fg_magenta} $ZSH_PROMPT_ASYNC_PATH already exists - skipping... ${reset}"
fi

ZSH_PROMPT_PURE_PATH=/usr/local/share/zsh/site-functions/prompt_pure_setup
if [ ! -f $ZSH_PROMPT_PURE_PATH ]; then
  echo "${fg_green} Linking $HOME/dotfiles/zsh-scripts/pure_pure.zsh -> $ZSH_PROMPT_PURE_PATH ${reset}"
  sudo ln -s $HOME/dotfiles/zsh-scripts/pure_pure.zsh $ZSH_PROMPT_PURE_PATH 
else
  echo "${fg_magenta} $ZSH_PROMPT_PURE_PATH already exists - skipping... ${reset}"
fi

if ! command -v fzf 
then 
  echo "${fg_red}fzf not found Please install it${fg_reset}"
  echo "Try:"
  echo "       git clone --depth 1 https://github.com/junegunn/fzf.git ~/.fzf && ~/.fzf/install"
  exit 1
fi


PROFILE_PATH=$HOME/.profile
if [ ! -f $PROFILE_PATH ]; then
  echo "${fg_green} Linking $HOME/dotfiles/.profile -> $PROFILE_PATH ${reset}"
  sudo ln -s $HOME/dotfiles/.profile $PROFILE_PATH 
else
  echo "${fg_magenta} $PROFILE_PATH already exists - skipping... ${reset}"
fi

echo "${fg_green}$ Setting zsh to default ${reset}"
chsh -s $(which zsh)


echo "${fg_green} Setting up for neovim ${reset}"

if ! command -v nvim && ! command -v nvim.appimage
then
  echo "${fg_red}nvim not found Please install it${fg_reset}"
  echo "Try:"
  echo "       https://github.com/neovim/neovim/releases"
  exit 1
fi

# vim.pack (the plugin manager) and nvim-treesitter `main` require Neovim >= 0.12.
NVIM_BIN="$(command -v nvim || command -v nvim.appimage)"
NVIM_VER="$("$NVIM_BIN" --version | head -1 | grep -oE '[0-9]+\.[0-9]+' | head -1)"
if [ "${NVIM_VER%%.*}" -eq 0 ] && [ "${NVIM_VER##*.}" -lt 12 ]; then
  echo "${fg_red}Neovim $NVIM_VER found, but >= 0.12 is required (vim.pack + treesitter main)${reset}"
  echo "Get a newer build at:"
  echo "       https://github.com/neovim/neovim/releases"
  exit 1
fi

# nvim-treesitter `main` compiles parsers locally; it needs the tree-sitter CLI
# (>= 0.26.1) and a C compiler on PATH.
if ! command -v tree-sitter
then
  echo "${fg_red}tree-sitter CLI not found - needed to build treesitter parsers${fg_reset}"
  echo "Try (prebuilt binary into ~/.local/bin):"
  echo "       curl -sL https://github.com/tree-sitter/tree-sitter/releases/latest/download/tree-sitter-linux-x64.gz | gunzip > ~/.local/bin/tree-sitter && chmod +x ~/.local/bin/tree-sitter"
  echo "${fg_magenta} (or: cargo install tree-sitter-cli) - must be >= 0.26.1, do NOT use the npm version ${reset}"
  exit 1
fi

if ! command -v cc && ! command -v gcc && ! command -v clang
then
  echo "${fg_red}No C compiler (cc/gcc/clang) found - needed to compile treesitter parsers${fg_reset}"
  echo "Try:"
  echo "       sudo apt-get install build-essential"
  exit 1
fi

# Python LSP tooling: basedpyright + ruff are installed as standalone, venv-agnostic
# tools via `uv` (they analyze each project's own venv). uv also builds the python3
# provider host venv below.
if ! command -v uv
then
  echo "${fg_red}uv not found - used to install the python LSP tools${fg_reset}"
  echo "Try:"
  echo "       curl -LsSf https://astral.sh/uv/install.sh | sh"
  exit 1
fi

if ! command -v basedpyright-langserver
then
  echo "${fg_red}basedpyright-langserver not found - python LSP (types/completion)${fg_reset}"
  echo "Try:"
  echo "       uv tool install basedpyright"
  exit 1
fi

if ! command -v ruff
then
  echo "${fg_red}ruff not found - python lint/format LSP${fg_reset}"
  echo "Try:"
  echo "       uv tool install ruff"
  exit 1
fi

# python3 provider host venv (pynvim) - used by chadtree and python remote plugins.
# Referenced by g:python3_host_prog in globals.vim.
NVIM_HOST_VENV=$HOME/.local/share/nvim-host-venv
if [ ! -x "$NVIM_HOST_VENV/bin/python" ]; then
  echo "${fg_green} Creating nvim python3 host venv (pynvim) at $NVIM_HOST_VENV ${reset}"
  uv venv "$NVIM_HOST_VENV" && uv pip install --python "$NVIM_HOST_VENV/bin/python" pynvim
else
  echo "${fg_magenta} $NVIM_HOST_VENV already exists - skipping... ${reset}"
fi

# Kotlin LSP (JetBrains' official, IntelliJ-based, bundles its own JBR). Extracted
# standalone under ~/.local/kotlin-lsp; the cmd path is set in lua/mlsp.lua.
# Optional - only needed for Kotlin development, so this does not exit on failure.
KOTLIN_LSP_BIN=$HOME/.local/kotlin-lsp/extension/server/bin/intellij-server
if [ ! -x "$KOTLIN_LSP_BIN" ]; then
  echo "${fg_red}Kotlin LSP not found (optional - only needed for Kotlin dev)${fg_reset}"
  echo "Install: grab the linux-amd64 'Standalone' .vsix from the latest release and unzip it:"
  echo "       https://github.com/Kotlin/kotlin-lsp/releases/latest"
  echo "       mkdir -p ~/.local/kotlin-lsp && unzip kotlin-server-*-linux-amd64.vsix -d ~/.local/kotlin-lsp"
  echo "       chmod +x ~/.local/kotlin-lsp/extension/server/bin/intellij-server ~/.local/kotlin-lsp/extension/server/jbr/bin/*"
else
  echo "${fg_magenta} Kotlin LSP present - skipping... ${reset}"
fi

NEOVIMRC_PATH=$HOME/.config/nvim/init.vim
if [ ! -f $NEOVIMRC_PATH ]; then
  echo "${fg_green} Linking $HOME/dotfiles/init.vim -> $NEOVIMRC_PATH ${reset}"
  ln -s $HOME/dotfiles/init.vim $NEOVIMRC_PATH 
else
  echo "${fg_magenta} $NEOVIMRC_PATH already exists - skipping... ${reset}"
fi

VIMRC_PATH=$HOME/.vimrc
if [ ! -f $VIMRC_PATH ]; then
  echo "${fg_green} Linking $HOME/dotfiles/.vimrc -> $VIMRC_PATH ${reset}"
  ln -s $HOME/dotfiles/.vimrc $VIMRC_PATH 
else
  echo "${fg_magenta} $VIMRC_PATH already exists - skipping... ${reset}"
fi

VIM_PATH=$HOME/.vim
if [ ! -f $VIM_PATH/colors/mycolo.vim ]; then
  echo "${fg_green} Linking $HOME/dotfiles/.vim -> $VIM_PATH ${reset}"
  ln -s $HOME/dotfiles/.vim $VIM_PATH 
else
  echo "${fg_magenta} $VIM_PATH already exists - skipping... ${reset}"
fi

VIM_LUA_PATH=$HOME/.config/nvim/lua
if [ ! -d $VIM_LUA_PATH ]; then
  echo "${fg_green} Linking $HOME/dotfiles/lua -> $VIM_LUA_PATH"
 ln -s $HOME/dotfiles/lua $HOME/.config/nvim/lua
fi

# Plugins are managed by Neovim's native vim.pack (Neovim 0.12+). They install
# automatically on first launch of nvim into ~/.local/share/nvim/site/pack.
echo "${fg_green} Plugins are managed by native vim.pack - launch nvim to install them ${reset}"
echo "${fg_magenta} Requires Neovim >= 0.12 ${reset}"


echo "${fg_green}$ Setting up for tmux ${reset}"

if ! command -v tmux 
then 
  echo "${fg_red}tmux not found Please install it${fg_reset}"
  echo "Try:"
  echo "       sudo apt-get install tmux"
  exit 1
fi


TMUXRC_PATH=$HOME/.tmux.conf
if [ ! -f $TMUXRC_PATH ]; then
  echo "${fg_green} Linking $HOME/dotfiles/.tmux.conf -> $TMUXRC_PATH ${reset}"
  ln -s $HOME/dotfiles/.tmux.conf $TMUXRC_PATH
else
  echo "${fg_magenta} $TMUXRC_PATH already exists - skipping... ${reset}"
fi 

echo "${fg_green}$ Setting up feh (Background image setter) ${reset}"

if ! command -v feh
then 
  echo "${fg_red}feh not found Please install it${fg_reset}"
  echo "try:"
  echo "       sudo apt-get install feh"
  exit 1
fi


echo "${fg_green}$ Setting up for xmobar ${reset}"

if ! command -v xmobar 
then 
  echo "${fg_red}xmobar not found Please install it${fg_reset}"
  echo "I am using 0.37 - Install from source here:"
  echo "       https://github.com/jaor/xmobar"
  echo "${fg_magenta} Remember to install with all extensions ${fg_reset}"
  exit 1
fi

XMOBAR_PATH=$HOME/.xmobarrc
if [ ! -f $XMOBAR_PATH ]; then
  echo "${fg_green} Linking $HOME/dotfiles/.xmobarrc -> $XMOBAR_PATH ${reset}"
  ln -s $HOME/dotfiles/.xmobarrc $XMOBAR_PATH
else
  echo "${fg_magenta} $XMOBAR_PATH already exists - skipping... ${reset}"
fi 

echo "${fg_green}$ Setting up for xmonad ${reset}"

if ! command -v xmonad 
then 
  echo "${fg_red}Xmonad not found Please install it${fg_reset}"
  echo "I am using 0.15 - See installation instructions  here:"
  echo "       https://github.com/xmonad/xmonad"
  echo "${fg_magenta} Make sure the version is >= 0.15 and make sure to install contrib ${fg_reset}"
  exit 1
fi

XMONAD_PATH=$HOME/.xmonad/xmonad.hs
if [ ! -f $XMONAD_PATH ]; then
  echo "${fg_green} Linking $HOME/dotfiles/xmonad.hs -> $XMONAD_PATH ${reset}"
  ln -s $HOME/dotfiles/xmonad.hs $XMONAD_PATH

  echo "${fg_green}$ You will have to recompile xmonad once this script is done.. ${reset}"
else
  echo "${fg_magenta} $XMONAD_PATH already exists - skipping... ${reset}"
fi 

CALC_PATH=/usr/bin/calc
if [ ! -f $CALC_PATH ]; then
  echo "${fg_green} Linking $HOME/dotfiles/bin/calc -> $CALC_PATH ${reset}"
  sudo ln -s $HOME/dotfiles/bin/calc $CALC_PATH
else
  echo "${fg_magenta} $CALC_PATH already exists - skipping... ${reset}"
fi 



XMONAD_XSESSION_PATH=/usr/share/xsessions/xmonad.desktop
if [ ! -f $XMONAD_XSESSION_PATH ]; then
  echo "${fg_green} Linking $HOME/dotfiles/xmonad.desktop -> $XMONAD_XSESSION_PATH ${reset}"
  sudo ln -s $PWD/xmonad.desktop /usr/share/xsessions/xmonad.desktop
else
  echo "${fg_magenta} $XMONAD_XSESSION_PATH already exists - skipping... ${reset}"
fi 

# TODO this might break it actually? just use the desktop thingy
#echo "${fg_green}$ Creating .xsessionrc that will start xmonad ${reset}"

#XSESSIONRC_PATH=$HOME/.xsessionrc
#if [ ! -f $XSESSIONRC_PATH ]; then
  #echo "${fg_green} Linking $HOME/dotfiles/.xsessionrc -> $XSESSIONRC_PATH ${reset}"
  #ln -s $HOME/dotfiles/.xsessionrc $XSESSIONRC_PATH

  #echo "${fg_green}$ If something breaks when you restart now - just enter a tty and comment out the contents of this create file ${reset}"
#else
  #echo "${fg_magenta} $XSESSIONRC_PATH already exists - skipping... ${reset}"
  #echo "${fg_magenta} Make sure the file launches xmonad.. ${reset}"
#fi 



echo "${fg_green}$ Setting up for nerdfonts ${reset}"
NERD_FONTS=$(fc-list | grep Nerd)
if [ -z $NERD_FONTS ] 
then 
  echo "${fg_red}nerds fonts not found Please install it${fg_reset}"
  echo "Try:"
  echo "       git clone --depth 1 https://github.com/ryanoasis/nerd-fonts.git ~/.nerd-fonts && ~/.nerd-fonts/install.sh"
  echo "${fg_red} This will take a while since it is a lot of fonts${fg_reset}"
  exit 1
else
  echo "${fg_magenta} Nerdfonts already exists - skipping... ${reset}"
fi

echo "${fg_green}$ Setting up GDM3 ${reset}"
echo "${fg_green}$ Running 'sudo dpkg-reconfigure gdm3' ${reset}"
sudo dpkg-reconfigure gdm3

