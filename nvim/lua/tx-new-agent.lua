-- tx-new-agent: the prefix+n form, in the key's tmux popup. `tx _new-agent` writes a JSON spec
-- and runs
--   nvim --clean -c "lua require('tx-new-agent').open()"
-- with the spec's path in $TX_NEW_AGENT_SPEC. Spawning writes the answers to spec.result as JSON
-- and quits; cancelling quits without writing it.
--
--   spec = { result, projects = { … }, project_paths = { [project] = path }, project,
--            engines = { … }, models = { [engine] = { … } }, efforts = { [engine] = { … } },
--            engine, effort, roles = { … }, role, tags_hint }
--
-- spec.project is preselected among the projects (the answers carry its path as cwd). Each model
-- list starts with the engine's default, which is what the form preselects; the roles start with
-- "none" (COMMON alone), and spec.role is preselected.
--
-- Panels: [1] Name  [2] Tags  [3] Project  [4] Prompt  [5] Engine  [6] Model  [7] Effort
-- [8] Role. The form opens in normal mode on Name (i to type), and moving between panels keeps
-- the mode.
-- Tab / S-Tab step through the panels in either mode. In normal mode h/l step too, 1-8 jump to
-- a panel, and on the text panels and Project j/k move to the panel below / above. In a list
-- the cursor line is the pick, so j/k or Up/Down choose, in either mode, wrapping at both ends.
-- Project is a one-line field beside Tags showing the picked project: Left/Right cycle through
-- them in either mode, and while it is active the help line shows its path. Titles are centred,
-- and Project's and Model's carry the pick's position after the label (like <leader>ac's
-- panels). Enter spawns from any panel; Esc leaves insert mode, and Esc or q in normal mode
-- cancels.

local M = {}

-- tx's palette (crates/tx/src/palette.rs, tmux/tx-ide.tmux)
local ACCENT = "#7aa2f7"
local FG = "#c0caf5"
local DIM = "#a9b1d6"
local WARN = "#e0af68"
local BORDER = "#3b4261"
local SELECTION = "#303030"

local HELP = " h/l j/k move · 1-8 jump · i type · j/k pick · ←/→ project · Enter spawn · Esc cancel"

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
    project = spec.project,
    engine = spec.engine,
    model = spec.models[spec.engine][1],
    effort = spec.effort,
    role = spec.role,
  }
  local function options(key)
    if key == "project" then
      return spec.projects
    elseif key == "engine" then
      return spec.engines
    elseif key == "role" then
      return spec.roles
    end
    return spec[key .. "s"][picked.engine]
  end

  -- Geometry: full-width text panels (Tags and Project split their row) around a row of four
  -- lists, Model as wide as its longest name and Role taking the rest; a help line under them.
  local width = vim.o.columns - 2
  local project_width = math.floor((width - 2) / 2)
  local tags_width = width - project_width - 2
  local engine_width, effort_width, model_width = 15, 12, 12
  for _, models in pairs(spec.models) do
    for _, model in ipairs(models) do
      model_width = math.max(model_width, #model + 4)
    end
  end
  local role_width = width - engine_width - model_width - effort_width - 6
  local list_height = math.max(#spec.engines, #spec.roles)
  for _, key in ipairs({ "models", "efforts" }) do
    for _, list in pairs(spec[key]) do
      list_height = math.max(list_height, #list)
    end
  end

  local panels = {
    { label = "Name", field = "name", row = 0, col = 0, width = width, height = 1, above = "Engine", below = "Tags" },
    {
      label = "Tags",
      field = "tags",
      row = 3,
      col = 0,
      width = tags_width,
      height = 1,
      hint = spec.tags_hint,
      above = "Name",
      below = "Prompt",
    },
    {
      label = "Project",
      key = "project",
      cycle = true,
      counter = true,
      above = "Name",
      below = "Prompt",
      row = 3,
      col = tags_width + 2,
      width = project_width,
      height = 1,
    },
    {
      label = "Prompt",
      field = "prompt",
      row = 6,
      col = 0,
      width = width,
      height = 4,
      wrap = true,
      above = "Tags",
      below = "Engine",
    },
    { label = "Engine", key = "engine", row = 12, col = 0, width = engine_width, height = list_height },
    {
      label = "Model",
      key = "model",
      counter = true,
      row = 12,
      col = engine_width + 2,
      width = model_width,
      height = list_height,
    },
    {
      label = "Effort",
      key = "effort",
      row = 12,
      col = engine_width + model_width + 4,
      width = effort_width,
      height = list_height,
    },
    {
      label = "Role",
      key = "role",
      row = 12,
      col = engine_width + model_width + effort_width + 6,
      width = role_width,
      height = list_height,
    },
  }
  -- the panel showing a text field or a list
  local function panel_for(name)
    for _, panel in ipairs(panels) do
      if panel.field == name or panel.key == name then
        return panel
      end
    end
  end
  local function panel_by_label(label)
    for _, panel in ipairs(panels) do
      if panel.label == label then
        return panel
      end
    end
  end

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
  local function help_for(panel)
    if panel.key == "project" then
      local path = vim.fn.fnamemodify(spec.project_paths[picked.project], ":~")
      say(" ←/→ pick · h/l move · Enter spawn · " .. path, "TxFormHelp")
    else
      say(HELP, "TxFormHelp")
    end
  end

  local active = 1
  -- A picker's place in its options, `index/count`.
  local function position(panel)
    local choices = options(panel.key)
    for index, option in ipairs(choices) do
      if option == picked[panel.key] then
        return ("%d/%d"):format(index, #choices)
      end
    end
  end
  -- The centred title: the label and, on a counting panel, the pick's position after it.
  local function decoration(panel, is_active)
    local group = is_active and "TxFormTitleActive" or "TxFormTitle"
    local text = (" [%d] %s "):format(panel.number, panel.label)
    if panel.counter then
      text = (" [%d] %s %s "):format(panel.number, panel.label, position(panel))
    end
    return { title = { { text, group } }, title_pos = "center" }
  end

  -- A one-line picker: the picked option between arrows, centred; its position is in the title.
  local function render_cycle(panel)
    local text = ("‹ %s ›"):format(picked[panel.key])
    local margin = math.floor((panel.width - vim.fn.strdisplaywidth(text)) / 2)
    vim.bo[panel.buffer].modifiable = true
    vim.api.nvim_buf_set_lines(panel.buffer, 0, -1, false, { (" "):rep(margin) .. text })
    vim.bo[panel.buffer].modifiable = false
    vim.api.nvim_buf_clear_namespace(panel.buffer, namespace, 0, -1)
    vim.api.nvim_buf_add_highlight(panel.buffer, namespace, "TxFormPicked", 0, 0, -1)
    vim.api.nvim_win_set_config(panel.window, decoration(panel, panel.number == active))
  end

  local function cycle(panel, step)
    local choices = options(panel.key)
    for index, option in ipairs(choices) do
      if option == picked[panel.key] then
        picked[panel.key] = choices[(index - 1 + step) % #choices + 1]
        break
      end
    end
    render_cycle(panel)
    help_for(panel)
  end

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
    if panel.counter then
      vim.api.nvim_win_set_config(panel.window, decoration(panel, panel.number == active))
    end
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
      render_list(panel_for("model"))
      render_list(panel_for("effort"))
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

  local function focus(number)
    local previous = panels[active]
    vim.api.nvim_win_set_config(previous.window, decoration(previous, false))
    vim.wo[previous.window].winhighlight = "FloatBorder:TxFormBorder"
    vim.wo[previous.window].cursorline = false
    active = number
    local panel = panels[number]
    vim.api.nvim_win_set_config(panel.window, decoration(panel, true))
    vim.wo[panel.window].winhighlight = "FloatBorder:TxFormBorderActive"
    vim.api.nvim_set_current_win(panel.window)
    help_for(panel)
    vim.wo[panel.window].cursorline = panel.key ~= nil and not panel.cycle
    -- The mode carries over; in a text panel the cursor goes to the end of the text.
    if not panel.key then
      local last = vim.api.nvim_buf_line_count(panel.buffer)
      local line = vim.api.nvim_buf_get_lines(panel.buffer, last - 1, last, false)[1]
      vim.api.nvim_win_set_cursor(panel.window, { last, #line })
    end
  end

  local function text(panel, separator)
    local lines = vim.api.nvim_buf_get_lines(panel.buffer, 0, -1, false)
    return vim.trim(table.concat(lines, separator))
  end

  local function spawn()
    local answers = {
      name = text(panel_for("name"), " "),
      prompt = text(panel_for("prompt"), "\n"),
      engine = picked.engine,
      model = picked.model,
      effort = picked.effort,
      role = picked.role,
      tags = text(panel_for("tags"), ","),
      cwd = spec.project_paths[picked.project],
    }
    for _, field in ipairs({ "name", "prompt" }) do
      if answers[field] == "" then
        local panel = panel_for(field)
        focus(panel.number)
        say((" %s cannot be empty."):format(panel.label), "TxFormWarning")
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
    panel.window = vim.api.nvim_open_win(
      panel.buffer,
      false,
      vim.tbl_extend("error", decoration(panel, false), {
        relative = "editor",
        row = panel.row,
        col = panel.col,
        width = panel.width,
        height = panel.height,
        style = "minimal",
        border = "rounded",
      })
    )
    vim.wo[panel.window].winhighlight = "FloatBorder:TxFormBorder"
    vim.wo[panel.window].wrap = panel.wrap or false
    vim.wo[panel.window].linebreak = panel.wrap or false
    if panel.cycle then
      render_cycle(panel)
    elseif panel.key then
      render_list(panel)
      vim.api.nvim_create_autocmd({ "CursorMoved", "CursorMovedI" }, {
        buffer = panel.buffer,
        callback = function()
          pick_at_cursor(panel)
        end,
      })
    elseif panel.hint then
      hint(panel)
      -- on every edit: TextChangedI is skipped when a key moves on before it fires
      vim.api.nvim_buf_attach(panel.buffer, false, {
        on_lines = function()
          vim.schedule(function()
            hint(panel)
          end)
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
    -- Normal mode: h / l step through the panels; on a panel that is not a list, j / k move to
    -- the one below / above.
    map("n", "l", function()
      focus(number % #panels + 1)
    end)
    map("n", "h", function()
      focus((number - 2) % #panels + 1)
    end)
    if panel.below then
      map("n", "j", function()
        focus(panel_by_label(panel.below).number)
      end)
      map("n", "k", function()
        focus(panel_by_label(panel.above).number)
      end)
    end
    if panel.cycle then
      map({ "n", "i" }, "<Right>", function()
        cycle(panel, 1)
      end)
      map({ "n", "i" }, "<Left>", function()
        cycle(panel, -1)
      end)
    elseif panel.key then
      -- The pick moves by one line, wrapping at both ends (in insert mode too: a list cannot
      -- take text).
      local function move(step)
        local count = #options(panel.key)
        local line = vim.api.nvim_win_get_cursor(panel.window)[1]
        vim.api.nvim_win_set_cursor(panel.window, { (line - 1 + step) % count + 1, 0 })
        pick_at_cursor(panel)
      end
      for _, keys in ipairs({ "j", "<Down>" }) do
        map({ "n", "i" }, keys, function()
          move(1)
        end)
      end
      for _, keys in ipairs({ "k", "<Up>" }) do
        map({ "n", "i" }, keys, function()
          move(-1)
        end)
      end
    end
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
end

return M
