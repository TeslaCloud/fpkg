-- fpkg's GLua runtime: console variables and the `cvars` library.
-- `CreateConVar` (flags as a number or a list of numbers; an existing convar is returned
-- as it is, a name taken by a console command gives nil), `CreateClientConVar`,
-- `GetConVar`, `GetConVar_Internal`, `ConVarExists`, `GetConVarNumber` and
-- `GetConVarString`. Names are case-insensitive. A convar is a `ConVar` object with the
-- methods of the class: the getters read numbers the way the engine does (the leading
-- number of the text, 0 when there is none; `GetInt` truncates, `GetBool` is `GetInt() ~= 0`),
-- the setters keep a numeric value within the bounds given at creation, and every change of
-- value calls `cvars.OnConVarChanged(name, old, new)` with the name the convar was created
-- with, which runs the callbacks `cvars.AddChangeCallback` added under exactly that name (one
-- per identifier; `RemoveChangeCallback`, `GetConVarCallbacks`). `cvars.Bool`, `Number` and
-- `String` read a convar as its `GetBool`, `GetFloat` and `GetString` do, or give the default
-- when there is no such convar.

local type, tostring, tonumber = type, tostring, tonumber
local lower, match = string.lower, string.match
local floor, ceil = math.floor, math.ceil
local bor, band = bit.bor, bit.band

cvars = cvars or {}

local CONVAR = FindMetaTable('ConVar')
local state_of = {}
local convar_named = {}

local function leading_number(text)
  local number = tonumber(text)

  if number then
    return number
  end

  local prefix = match(text, '^%s*[%+%-]?%d*%.?%d*[eE][%+%-]?%d+') or match(text, '^%s*[%+%-]?%d*%.?%d*')

  return tonumber(prefix) or 0
end

local function number_text(number)
  if number == floor(number) and number > -1e15 and number < 1e15 then
    return string.format('%d', number)
  end

  return tostring(number)
end

local function bounded(state, text)
  if state.min == nil and state.max == nil then
    return text
  end

  local number = leading_number(text)
  local kept = number

  if state.min ~= nil and kept < state.min then
    kept = state.min
  end

  if state.max ~= nil and kept > state.max then
    kept = state.max
  end

  if kept ~= number then
    return number_text(kept)
  end

  return text
end

local function assign(self, text)
  local state = state_of[self]
  local old = state.value
  local new = bounded(state, tostring(text))

  if new == old then
    return
  end

  state.value = new
  cvars.OnConVarChanged(state.name, old, new)
end

function CONVAR:GetName()
  return state_of[self].name
end

function CONVAR:GetString()
  return state_of[self].value
end

function CONVAR:GetFloat()
  return leading_number(state_of[self].value)
end

function CONVAR:GetInt()
  local number = leading_number(state_of[self].value)

  if number < 0 then
    return ceil(number)
  end

  return floor(number)
end

function CONVAR:GetBool()
  return self:GetInt() ~= 0
end

function CONVAR:GetDefault()
  return state_of[self].default
end

function CONVAR:GetHelpText()
  return state_of[self].help
end

function CONVAR:GetFlags()
  return state_of[self].flags
end

function CONVAR:IsFlagSet(flag)
  return band(state_of[self].flags, flag) ~= 0
end

function CONVAR:GetMin()
  return state_of[self].min
end

function CONVAR:GetMax()
  return state_of[self].max
end

function CONVAR:SetString(value)
  assign(self, value)
end

function CONVAR:SetFloat(value)
  assign(self, tostring(tonumber(value) or 0))
end

function CONVAR:SetInt(value)
  local number = tonumber(value) or 0

  if number < 0 then
    number = ceil(number)
  else
    number = floor(number)
  end

  assign(self, number_text(number))
end

function CONVAR:SetBool(value)
  assign(self, value and '1' or '0')
end

function CONVAR:Revert()
  assign(self, state_of[self].default)
end

CONVAR.__tostring = function(self)
  return 'ConVar ['..state_of[self].name..']'
end

local function flags_of(flags)
  if type(flags) == 'table' then
    local combined = 0

    for _, flag in ipairs(flags) do
      combined = bor(combined, flag)
    end

    return combined
  end

  return tonumber(flags) or 0
end

local function check_name(name, func_name)
  if type(name) ~= 'string' then
    error("bad argument #1 to '"..func_name.."' (string expected, got "..type(name)..')', 3)
  end
end

function CreateConVar(name, value, flags, help, min, max)
  check_name(name, 'CreateConVar')

  local key = lower(name)
  local existing = convar_named[key]

  if existing then
    return existing
  end

  if concommand.GetTable()[key] ~= nil then
    return nil
  end

  local text = tostring(value)
  local convar = newproxy()

  debug.setmetatable(convar, CONVAR)
  state_of[convar] = {
    name = name,
    default = text,
    value = text,
    flags = flags_of(flags),
    help = help ~= nil and tostring(help) or '',
    min = tonumber(min),
    max = tonumber(max)
  }
  state_of[convar].value = bounded(state_of[convar], text)
  convar_named[key] = convar

  return convar
end

function CreateClientConVar(name, default, should_save, userinfo, help, min, max)
  local flags = 0

  if should_save ~= false then
    flags = bor(flags, FCVAR_ARCHIVE)
  end

  if userinfo then
    flags = bor(flags, FCVAR_USERINFO)
  end

  return CreateConVar(name, default, flags, help, min, max)
end

function GetConVar(name)
  check_name(name, 'GetConVar')

  return convar_named[lower(name)]
end

GetConVar_Internal = GetConVar

function ConVarExists(name)
  check_name(name, 'ConVarExists')

  return convar_named[lower(name)] ~= nil
end

function GetConVarNumber(name)
  check_name(name, 'GetConVarNumber')

  local convar = convar_named[lower(name)]

  if convar == nil then
    return 0
  end

  return convar:GetFloat()
end

function GetConVarString(name)
  check_name(name, 'GetConVarString')

  local convar = convar_named[lower(name)]

  if convar == nil then
    return ''
  end

  return convar:GetString()
end

local callbacks_of = {}

function cvars.GetConVarCallbacks(name, create)
  local key = tostring(name)
  local list = callbacks_of[key]

  if list == nil and create then
    list = {}
    callbacks_of[key] = list
  end

  return list
end

local function entry_parts(entry)
  if type(entry) == 'table' then
    return entry[1], entry[2]
  end

  return entry, nil
end

function cvars.AddChangeCallback(name, callback, identifier)
  local list = cvars.GetConVarCallbacks(name, true)

  if identifier == nil then
    list[#list + 1] = callback

    return
  end

  for _, entry in ipairs(list) do
    if type(entry) == 'table' and entry[2] == identifier then
      entry[1] = callback

      return
    end
  end

  list[#list + 1] = { callback, identifier }
end

function cvars.RemoveChangeCallback(name, identifier)
  local list = callbacks_of[tostring(name)]

  if list == nil then
    return
  end

  for i = #list, 1, -1 do
    local _, entry_identifier = entry_parts(list[i])

    if entry_identifier ~= nil and entry_identifier == identifier then
      table.remove(list, i)
    end
  end
end

function cvars.OnConVarChanged(name, old, new)
  local list = callbacks_of[tostring(name)]

  if list == nil then
    return
  end

  local snapshot = {}

  for i, entry in ipairs(list) do
    snapshot[i] = entry_parts(entry)
  end

  for _, callback in ipairs(snapshot) do
    callback(name, old, new)
  end
end

function cvars.String(name, default)
  local convar = GetConVar(name)

  if convar == nil then
    return default
  end

  return convar:GetString()
end

function cvars.Number(name, default)
  local convar = GetConVar(name)

  if convar == nil then
    return default
  end

  return convar:GetFloat()
end

function cvars.Bool(name, default)
  local convar = GetConVar(name)

  if convar == nil then
    return default
  end

  return convar:GetBool()
end
