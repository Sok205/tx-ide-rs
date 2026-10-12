-- tx-new-agent: the prefix+n form, in the key's tmux popup. `tx _new-agent` writes a JSON spec
-- and runs
--   nvim --clean -c "lua require('tx-new-agent').open()"
-- with the spec's path in $TX_NEW_AGENT_SPEC. Spawning writes the answers to spec.result as JSON
-- and quits; cancelling quits without writing it.
--
--   spec = { result, engines = { … }, models = { [engine] = { … } }, efforts = { [engine] = { … } },
--            engine, effort, tags_hint }
--
-- Each model list starts with the engine's default, which is what the form preselects.
--
-- Panels: [1] Name  [2] Prompt  [3] Engine  [4] Model  [5] Effort  [6] Tags. The form opens in
-- insert mode on Name; moving to a panel lands in normal mode. Tab / S-Tab step through the
-- panels and 1-6 jump to one (normal mode); in a list the cursor line is the pick, so j/k or the
-- arrows choose. Enter spawns from any panel; Esc leaves insert mode, and Esc or q in normal mode
-- cancels.

local M = {}

-- tx's palette (crates/tx/src/palette.rs, tmux/tx-ide.tmux)
local ACCENT = "#7aa2f7"
local FG = "#c0caf5"
local DIM = "#a9b1d6"
local WARN = "#e0af68"
local BORDER = "#3b4261"
local SELECTION = "#303030"

local HELP = " Tab next · 1-6 jump · j/k pick · Enter spawn · Esc cancel"

local function highlights()
  local set = vim.api.nvim_set_hl
  set(0, "Normal", { fg = FG })
  set(0, "NormalFloat", { fg = FG })
  set(0, "TxFormBorder", { fg = BORDER })
  set(0, "TxFormBorderActive", { fg = ACCENT })
  set(0, "TxFormTitle", { fg = DIM })
  set(0, "TxFormTitleActive", { fg = ACCENT, bold = true })
  set(0, "TxFormPicked", { fg = ACCENT, bold = true })
  set(0, "TxFormHint", { fg = DIM, italic = true })
  set(0, "TxFormHelp", { fg = DIM })
  set(0, "TxFormWarning", { fg = WARN })
  set(0, "CursorLine", { bg = SELECTION })
end

function M.open()
  local file = assert(io.open(vim.env.TX_NEW_AGENT_SPEC))
  local spec = vim.json.decode(file:read("*a"))
  file:close()

  vim.o.termguicolors = true
  vim.o.laststatus = 0
  vim.o.ruler = false
  vim.o.mouse = "a"
  highlights()

  local namespace = vim.api.nvim_create_namespace("tx_new_agent")
  local picked = {
    engine = spec.engine,
    model = spec.models[spec.engine][1],
    effort = spec.effort,
  }
  local function options(key)
    if key == "engine" then
      return spec.engines
    end
    return spec[key .. "s"][picked.engine]
  end

  -- Geometry: full-width text panels around a row of three lists; a help line under them.
  local width = vim.o.columns - 2
  local engine_width, effort_width = 15, 12
  local model_width = width - engine_width - effort_width - 4
  local list_height = 0
  for _, key in ipairs({ "models", "efforts" }) do
    for _, list in pairs(spec[key]) do
      list_height = math.max(list_height, #list)
    end
  end
  list_height = math.max(list_height, #spec.engines)

  local panels = {
    { label = "Name", row = 0, col = 0, width = width, height = 1 },
    { label = "Prompt", row = 3, col = 0, width = width, height = 4, wrap = true },
    { label = "Engine", key = "engine", row = 9, col = 0, width = engine_width, height = list_height },
    {
      label = "Model",
      key = "model",
      row = 9,
      col = engine_width + 2,
      width = model_width,
      height = list_height,
    },
    {
      label = "Effort",
      key = "effort",
      row = 9,
      col = engine_width + model_width + 4,
      width = effort_width,
      height = list_height,
    },
    { label = "Tags", row = list_height + 11, col = 0, width = width, height = 1, hint = spec.tags_hint },
  }

  local help_buffer = vim.api.nvim_create_buf(false, true)
  local help_window = vim.api.nvim_open_win(help_buffer, false, {
    relative = "editor",
    row = list_height + 14,
    col = 0,
    width = width + 2,
    height = 1,
    style = "minimal",
    focusable = false,
  })
  local function say(message, group)
    vim.api.nvim_buf_set_lines(help_buffer, 0, -1, false, { message })
    vim.api.nvim_buf_clear_namespace(help_buffer, namespace, 0, -1)
    vim.api.nvim_buf_add_highlight(help_buffer, namespace, group, 0, 0, -1)
  end
  say(HELP, "TxFormHelp")

  local function render_list(panel)
    local lines = {}
    local picked_line = 1
    for line, option in ipairs(options(panel.key)) do
      local mark = option == picked[panel.key] and "●" or " "
      lines[line] = (" %s %s"):format(mark, option)
      if option == picked[panel.key] then
        picked_line = line
      end
    end
    vim.bo[panel.buffer].modifiable = true
    vim.api.nvim_buf_set_lines(panel.buffer, 0, -1, false, lines)
    vim.bo[panel.buffer].modifiable = false
    vim.api.nvim_buf_clear_namespace(panel.buffer, namespace, 0, -1)
    vim.api.nvim_buf_add_highlight(panel.buffer, namespace, "TxFormPicked", picked_line - 1, 0, -1)
    vim.api.nvim_win_set_cursor(panel.window, { picked_line, 0 })
  end

  -- The cursor line is the pick; a new engine keeps the model and effort it offers, else its first.
  local function pick_at_cursor(panel)
    local line = vim.api.nvim_win_get_cursor(panel.window)[1]
    local option = options(panel.key)[line]
    if option == picked[panel.key] then
      return
    end
    picked[panel.key] = option
    if panel.key == "engine" then
      for _, key in ipairs({ "model", "effort" }) do
        if not vim.tbl_contains(options(key), picked[key]) then
          picked[key] = options(key)[1]
        end
      end
      render_list(panels[4])
      render_list(panels[5])
    end
    render_list(panel)
  end

  local function hint(panel)
    vim.api.nvim_buf_clear_namespace(panel.buffer, namespace, 0, -1)
    local text = table.concat(vim.api.nvim_buf_get_lines(panel.buffer, 0, -1, false), "")
    if text == "" then
      vim.api.nvim_buf_set_extmark(panel.buffer, namespace, 0, 0, {
        virt_text = { { panel.hint, "TxFormHint" } },
        virt_text_pos = "overlay",
      })
    end
  end

  local active = 1
  local function title(panel, is_active)
    return { { (" [%d] %s "):format(panel.number, panel.label), is_active and "TxFormTitleActive" or "TxFormTitle" } }
  end
  local function focus(number)
    local previous = panels[active]
    vim.api.nvim_win_set_config(previous.window, { title = title(previous, false), title_pos = "left" })
    vim.wo[previous.window].winhighlight = "FloatBorder:TxFormBorder"
    vim.wo[previous.window].cursorline = false
    active = number
    local panel = panels[number]
    vim.api.nvim_win_set_config(panel.window, { title = title(panel, true), title_pos = "left" })
    vim.wo[panel.window].winhighlight = "FloatBorder:TxFormBorderActive"
    vim.api.nvim_set_current_win(panel.window)
    say(HELP, "TxFormHelp")
    vim.wo[panel.window].cursorline = panel.key ~= nil
    -- Moving lands in normal mode; only opening the form starts in insert mode.
    vim.cmd("stopinsert")
  end

  local function text(panel, separator)
    local lines = vim.api.nvim_buf_get_lines(panel.buffer, 0, -1, false)
    return vim.trim(table.concat(lines, separator))
  end

  local function spawn()
    local answers = {
      name = text(panels[1], " "),
      prompt = text(panels[2], "\n"),
      engine = picked.engine,
      model = picked.model,
      effort = picked.effort,
      tags = text(panels[6], ","),
    }
    for number, key in ipairs({ "name", "prompt" }) do
      if answers[key] == "" then
        focus(number)
        say((" %s cannot be empty."):format(panels[number].label), "TxFormWarning")
        return
      end
    end
    local result = assert(io.open(spec.result, "w"))
    result:write(vim.json.encode(answers))
    result:close()
    vim.cmd("qall!")
  end

  for number, panel in ipairs(panels) do
    panel.number = number
    panel.buffer = vim.api.nvim_create_buf(false, true)
    panel.window = vim.api.nvim_open_win(panel.buffer, false, {
      relative = "editor",
      row = panel.row,
      col = panel.col,
      width = panel.width,
      height = panel.height,
      style = "minimal",
      border = "rounded",
      title = title(panel, false),
      title_pos = "left",
    })
    vim.wo[panel.window].winhighlight = "FloatBorder:TxFormBorder"
    vim.wo[panel.window].wrap = panel.wrap or false
    vim.wo[panel.window].linebreak = panel.wrap or false
    if panel.key then
      render_list(panel)
      vim.api.nvim_create_autocmd("CursorMoved", {
        buffer = panel.buffer,
        callback = function()
          pick_at_cursor(panel)
        end,
      })
    elseif panel.hint then
      hint(panel)
      vim.api.nvim_create_autocmd({ "TextChanged", "TextChangedI" }, {
        buffer = panel.buffer,
        callback = function()
          hint(panel)
        end,
      })
    end

    local function map(modes, keys, callback)
      vim.keymap.set(modes, keys, callback, { buffer = panel.buffer, nowait = true })
    end
    map({ "n", "i" }, "<Tab>", function()
      focus(number % #panels + 1)
    end)
    map({ "n", "i" }, "<S-Tab>", function()
      focus((number - 2) % #panels + 1)
    end)
    for target = 1, #panels do
      map("n", tostring(target), function()
        focus(target)
      end)
    end
    map({ "n", "i" }, "<CR>", spawn)
    map("n", "<Esc>", function()
      vim.cmd("qall!")
    end)
    map("n", "q", function()
      vim.cmd("qall!")
    end)
  end

  -- A click outside the panels lands on the empty editor window: send it back.
  vim.api.nvim_create_autocmd("WinEnter", {
    callback = function()
      local current = vim.api.nvim_get_current_win()
      for number, panel in ipairs(panels) do
        if panel.window == current then
          if number ~= active then
            focus(number)
          end
          return
        end
      end
      focus(active)
    end,
  })

  focus(1)
  vim.cmd("startinsert")
end

return M
