local null_ls = require("null-ls")

local sources = {
    null_ls.builtins.formatting.isort,
    null_ls.builtins.formatting.black,
}

null_ls.setup({
    sources = sources,
})

