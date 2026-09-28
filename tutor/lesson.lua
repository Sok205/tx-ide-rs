-- tx tutor: lesson buffers in the tutor view's nvim (loaded with `--cmd luafile`, before the
-- user's config). Globals set by `tx tutor start` just before loading this file:
--   vim.g.tx_tutor_lessons  autocmd pattern matching the pack's lesson files
--   vim.g.tx_tutor_shell    tmux target of the view's shell pane
-- Lesson buffers get no diagnostics (read-only docs), and <CR> types the command under the
-- cursor into the shell pane without running it.

-- The command under the cursor: the `code span` it sits in, else the whole line when inside a
-- fenced block; nil on prose.
function _G.tx_tutor_command()
  local row, col = unpack(vim.api.nvim_win_get_cursor(0))
  local line = vim.api.nvim_get_current_line()
  local start = 1
  while true do
    local open, close = line:find("`[^`]+`", start)
    if not open then
      break
    end
    if col + 1 >= open and col + 1 <= close then
      return line:sub(open + 1, close - 1)
    end
    start = close + 1
  end
  local fences = 0
  for _, above in ipairs(vim.api.nvim_buf_get_lines(0, 0, row - 1, false)) do
    if above:match("^%s*```") then
      fences = fences + 1
    end
  end
  local text = vim.trim(line)
  if fences % 2 == 1 and text ~= "" and not text:match("^```") then
    return text
  end
  return nil
end

local function type_into_shell()
  local command = _G.tx_tutor_command()
  if not command then
    vim.notify("tx tutor: put the cursor on a `command` (or a line in a code block)")
    return
  end
  vim.system({ "tmux", "send-keys", "-t", vim.g.tx_tutor_shell, "-l", "--", command })
end

if vim.g.tx_tutor_lessons then
  vim.api.nvim_create_autocmd("BufEnter", {
    group = vim.api.nvim_create_augroup("tx-tutor-lessons", { clear = true }),
    pattern = vim.g.tx_tutor_lessons,
    callback = function(args)
      vim.diagnostic.enable(false, { bufnr = args.buf })
      vim.keymap.set("n", "<CR>", type_into_shell, {
        buffer = args.buf,
        desc = "tx tutor: type this command into the shell",
      })
    end,
  })
end
