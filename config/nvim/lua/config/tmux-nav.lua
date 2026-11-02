-- <C-h/j/k/l>: nvim splits first, then tmux panes — the nvim half of the IDE's navigation
-- keys (tx-client's keys/ide.tmux). At the tabpage edge, hand off to tmux-nav, which walks out
-- of nested client-in-a-pane sessions.

local M = {}

-- `txc ui install` writes tmux-nav under <data>/bin; $PATH fallback when it is not installed.
local tx_data_directory = vim.env.TX_DATA_DIR
  or (vim.env.XDG_DATA_HOME or vim.env.HOME .. "/.local/share") .. "/tx-ide"
local navigator = vim.uv.fs_realpath(tx_data_directory .. "/bin/tmux-nav") or "tmux-nav"

local directions = {
  h = { flag = "L", name = "left" },
  j = { flag = "D", name = "lower" },
  k = { flag = "U", name = "upper" },
  l = { flag = "R", name = "right" },
}

local function navigate(key)
  local window = vim.api.nvim_get_current_win()
  vim.cmd.wincmd(key)
  if vim.api.nvim_get_current_win() ~= window then
    return
  end
  if vim.env.TMUX then
    vim.system({ navigator, directions[key].flag, vim.env.TMUX_PANE })
  end
end

function M.setup()
  for key, direction in pairs(directions) do
    vim.keymap.set({ "n", "t" }, "<C-" .. key .. ">", function()
      navigate(key)
    end, { desc = "Go to " .. direction.name .. " window/pane" })
  end
end

return M
