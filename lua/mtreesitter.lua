-- nvim-treesitter `main` branch (the Neovim 0.12 rewrite).
--
-- On `main`, this plugin only installs parsers and ships queries -- the actual
-- highlighting is done by Neovim's *native* treesitter (`vim.treesitter.start`).
-- The old `master` branch is frozen and incompatible with Neovim 0.12.
--
-- Requires the `tree-sitter` CLI and a C compiler on PATH to build parsers.

local ts = require("nvim-treesitter")

-- Parsers to keep installed. lua / vim / markdown / markdown_inline ship with
-- Neovim itself, so we only fetch the languages I actually use. install() is
-- idempotent: it no-ops for parsers that are already built, and self-heals a
-- fresh machine by compiling any that are missing (asynchronously).
local ensure = { "rust", "python", "kotlin", "swift" }
pcall(function() ts.install(ensure) end)

-- Turn on native treesitter highlighting for any buffer whose parser is
-- available. The guard isn't error-swallowing: a missing parser for some
-- random filetype is an expected, normal case -- we just skip highlighting it
-- rather than enabling something that can't work.
vim.api.nvim_create_autocmd("FileType", {
  callback = function(args)
    local lang = vim.treesitter.language.get_lang(vim.bo[args.buf].filetype)
    if not lang then
      return
    end
    local ok, available = pcall(vim.treesitter.language.add, lang)
    if ok and available then
      vim.treesitter.start(args.buf)
    end
  end,
})
