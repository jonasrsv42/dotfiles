
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

local builtin = require('telescope.builtin')


vim.keymap.set('n', '<leader>f', '<cmd>Telescope find_files<cr>', {silent = True, noremap = True})
vim.keymap.set('n', '<leader>g', '<cmd>Telescope live_grep<cr>', {silent = True, noremap = True})

vim.keymap.set('n', '<leader>G', function() 
  builtin.live_grep({ default_text = vim.fn.expand('<cword>') })
end, { desc = '[g]rep [l]ive [w]ord' })

vim.keymap.set('n', '<leader>t', '<cmd>Telescope<cr>', {silent = True, noremap = True})
vim.keymap.set('n', '<leader>p', '<cmd>Telescope oldfiles<cr>', {silent = True, noremap = True})



