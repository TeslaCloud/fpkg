-- fpkg's GLua runtime: console commands and the console itself.
-- `concommand.Add` (a callback that is not a function is reported, and kept), `Remove`,
-- `GetTable` (the callbacks and the completions by name), `Run` (the callback of a command,
-- `false` when there is none) and `AutoComplete`; names are case-insensitive, and a name taken
-- by a convar is refused. The console runs a line like this: commands separated by `;` or new
-- lines, each split into words at whitespace with double quotes grouping (only those two
-- split arguments here; the engine's own tokenizer could not be checked against a server),
-- its argument string being the rest of the command after its name without the surrounding
-- whitespace; a command name runs its callback with no player (`nil`),
-- a convar name sets the convar (to its one argument, or to the whole argument string) or
-- prints it, and anything else is reported as doing nothing here.
-- `concommand.__fpkg_run_line(line)` is that console (what the host runs), and
-- `RunConsoleCommand` and `game.ConsoleCommand` run commands through it.

local type, tostring, select = type, tostring, select
local find, sub, lower, byte = string.find, string.sub, string.lower, string.byte
local concat = table.concat

concommand = concommand or {}

local callbacks = {}
local completions = {}

function concommand.Add(name, callback, autocomplete)
  local key = lower(name)

  if type(callback) ~= 'function' then
    ErrorNoHaltWithStack("bad argument #2 to 'Add' (function expected, got "..type(callback)..')')
  end

  if ConVarExists and ConVarExists(key) then
    return
  end

  callbacks[key] = callback
  completions[key] = autocomplete
end

function concommand.Remove(name)
  local key = lower(name)

  callbacks[key] = nil
  completions[key] = nil
end

function concommand.GetTable()
  return callbacks, completions
end

function concommand.Run(actor, command, args, args_text)
  local callback = callbacks[lower(tostring(command))]

  if callback == nil then
    return false
  end

  callback(actor, command, args, args_text)

  return true
end

local function words_of(text)
  local words, ends = {}, {}
  local position, length = 1, #text

  while true do
    local start = find(text, '%S', position)

    if not start then
      break
    end

    local stop

    if byte(text, start) == 34 then
      local close = find(text, '"', start + 1, true) or length + 1

      words[#words + 1] = sub(text, start + 1, close - 1)
      stop = close
    else
      stop = (find(text, '[%s"]', start) or length + 1) - 1
      words[#words + 1] = sub(text, start, stop)
    end

    ends[#ends + 1] = stop
    position = stop + 1
  end

  return words, ends
end

local function trimmed(text)
  return (text:gsub('^%s+', ''):gsub('%s+$', ''))
end

function concommand.AutoComplete(command, arguments, ...)
  local complete = completions[lower(tostring(command))]

  if complete ~= nil then
    return complete(command, arguments, ...)
  end
end

local function show_convar(convar)
  local text = '"'..convar:GetName()..'" = "'..convar:GetString()..'" ( def. "'..convar:GetDefault()..'" )'
  local help = convar:GetHelpText()

  if help ~= '' then
    text = text..'\n - '..help
  end

  MsgN(text)
end

local function dispatch(name, args, args_text)
  if concommand.Run(nil, name, args, args_text) then
    return true
  end

  local convar = GetConVar and GetConVar(name)

  if convar then
    if #args == 0 then
      show_convar(convar)
    elseif #args == 1 then
      convar:SetString(args[1])
    else
      convar:SetString(args_text)
    end

    return true
  end

  ErrorNoHalt("'"..name.."' does nothing here: fpkg's runtime has no console command or convar of that name\n")

  return false
end

local function statements_of(line)
  local list = {}
  local first, quoted = 1, false

  for i = 1, #line do
    local c = byte(line, i)

    if c == 34 then
      quoted = not quoted
    elseif c == 10 or (c == 59 and not quoted) then
      list[#list + 1] = sub(line, first, i - 1)
      first = i + 1
      quoted = false
    end
  end

  list[#list + 1] = sub(line, first)

  return list
end

local function run_statement(statement)
  local words, ends = words_of(statement)

  if #words == 0 then
    return nil
  end

  local name = words[1]
  local args = {}

  for i = 2, #words do
    args[i - 1] = words[i]
  end

  return dispatch(name, args, trimmed(sub(statement, ends[1] + 1)))
end

function concommand.__fpkg_run_line(line)
  local ran, all_handled = false, true

  for _, statement in ipairs(statements_of(tostring(line))) do
    local handled = run_statement(statement)

    if handled ~= nil then
      ran = true
      all_handled = all_handled and handled
    end
  end

  return ran and all_handled
end

local function as_word(value)
  local text = tostring(value)

  if text == '' or find(text, '[%s;"]') then
    return '"'..text..'"'
  end

  return text
end

function RunConsoleCommand(command, ...)
  if type(command) ~= 'string' then
    error("bad argument #1 to 'RunConsoleCommand' (string expected, got "..type(command)..')', 2)
  end

  local args, shown = {}, {}

  for i = 1, select('#', ...) do
    local value = select(i, ...)

    args[i] = tostring(value)
    shown[i] = as_word(value)
  end

  dispatch(command, args, concat(shown, ' '))
end

game = game or {}

function game.ConsoleCommand(text)
  concommand.__fpkg_run_line(text)
end
