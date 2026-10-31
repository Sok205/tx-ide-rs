-- tx-client's nvim side, installed by `txc ui install` under <data>/nvim and put first on the
-- runtimepath of every nvim view tx-client starts. Activates the modules that work on their
-- own; tx-tour is required by whoever opens a tour. An operator config that already loads its
-- own copies (config.diffview-return, config.tx-ask) keeps them, so keymaps register once.
if not package.loaded["config.diffview-return"] then
  require("tx-diffview-return").setup()
end

if not (vim.g.tx_ask_loaded or package.loaded["config.tx-ask"]) then
  vim.g.tx_ask_loaded = true
  require("tx-ask")
end
