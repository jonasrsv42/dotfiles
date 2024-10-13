
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
      pylint = { enabled = false, executable = "pylint" },
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

vim.keymap.set('n', '<leader>F', vim.lsp.buf.format, {silent = True, noremap = True})
vim.keymap.set('n', 'gd', vim.lsp.buf.definition, {silent = True, noremap = True})
vim.keymap.set('n', 'K', vim.lsp.buf.hover, {silent = True, noremap = True})
vim.keymap.set('n', 'gr', vim.lsp.buf.references, {silent = True, noremap = True})
vim.keymap.set('n', '<C-space>', vim.lsp.buf.code_action, {silent = True, noremap = True})
vim.keymap.set('n', '<C-s>', vim.lsp.buf.signature_help, {silent = True, noremap = True})

