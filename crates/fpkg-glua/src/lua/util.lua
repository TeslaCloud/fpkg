-- fpkg's GLua runtime: the type functions and the general helpers of a server state.
-- `type` (an engine object gives its class name, other userdata "UserData"), `TypeID` and the
-- `is*` checks; `IsValid`, `tobool` (also `util.tobool`), `Format`, `Either`, `Lerp`,
-- `STNDRD`, `TimedSin`, `TimedCos`, `PrintTable` (a table met again inside itself is printed
-- as its address), `AccessorFunc` with the `FORCE_` enumeration, `RandomPairs`,
-- `ProtectedCall`, `IncludeCS`, `Add_NPC_Class` (on `NUM_AI_CLASSES`), `IsEnemyEntityName`,
-- `IsFriendEntityName`, `SafeRemoveEntity`, `SafeRemoveEntityDelayed`,
-- `IsTableOfEntitiesValid` and `debug.Trace`; `util.Stack`, `util.Timer` (on `CurTime`),
-- `util.NiceFloat`, `util.DateStamp` ("2026-3-7 04-05-06"), `util.TypeToString`,
-- `util.StringToType`, `util.IsBinaryModuleInstalled`; and the persistent player data in the
-- `playerpdata` table of the database (`util.GetPData`, `SetPData`, `RemovePData`, and the
-- `Player` methods of the same names, whose setters tell whether they succeeded), the table
-- being created when it is missing.

local raw_type = type
local raw_get = rawget
local meta_of = debug.getmetatable
local format = string.format

local function class_of(value)
  local meta = meta_of(value)

  if raw_type(meta) == 'table' then
    return meta
  end
end

function type(value)
  local kind = raw_type(value)

  if kind == 'userdata' then
    local meta = class_of(value)
    local name = meta and raw_get(meta, 'MetaName')

    if raw_type(name) == 'string' then
      return name
    end

    return 'UserData'
  end

  return kind
end

local ids_by_kind = {
  ['nil'] = TYPE_NIL,
  boolean = TYPE_BOOL,
  number = TYPE_NUMBER,
  string = TYPE_STRING,
  table = TYPE_TABLE,
  ['function'] = TYPE_FUNCTION,
  thread = TYPE_THREAD
}

function TypeID(value)
  local kind = raw_type(value)

  if kind ~= 'userdata' then
    return ids_by_kind[kind]
  end

  local meta = class_of(value)
  local id = meta and raw_get(meta, 'MetaID')

  if raw_type(id) == 'number' then
    return id
  end

  return TYPE_USERDATA
end

local function kind_check(kind)
  return function(value)
    return raw_type(value) == kind
  end
end

isstring = kind_check('string')
isnumber = kind_check('number')
isbool = kind_check('boolean')
istable = kind_check('table')
isfunction = kind_check('function')

local function class_check(id)
  return function(value)
    return raw_type(value) == 'userdata' and TypeID(value) == id
  end
end

isentity = class_check(TYPE_ENTITY)
IsEntity = isentity
isvector = class_check(TYPE_VECTOR)
isangle = class_check(TYPE_ANGLE)
ismatrix = class_check(TYPE_MATRIX)
ispanel = class_check(TYPE_PANEL)

function IsValid(object)
  if not object then
    return false
  end

  local check = object.IsValid

  if not check then
    return false
  end

  return check(object)
end

local false_values = { [0] = true, ['0'] = true, ['false'] = true }

function tobool(value)
  if value == nil or value == false then
    return false
  end

  return not false_values[value]
end

util.tobool = tobool

Format = string.format

function Either(condition, if_true, if_false)
  if condition then
    return if_true
  end

  return if_false
end

function Lerp(fraction, from, to)
  if fraction > 1 then
    return to
  elseif fraction < 0 then
    return from
  end

  return from + (to - from) * fraction
end

function STNDRD(number)
  local tens = number % 100

  if tens < 11 or tens > 13 then
    local last = number % 10

    if last == 1 then
      return 'st'
    elseif last == 2 then
      return 'nd'
    elseif last == 3 then
      return 'rd'
    end
  end

  return 'th'
end

local function wave(shape)
  return function(frequency, origin, max, offset)
    return origin + shape(CurTime() * frequency * math.pi * 2 + offset) * (max - origin) / 2
  end
end

TimedSin = wave(math.sin)
TimedCos = wave(math.cos)

local function key_order(a, b)
  local ta, tb = raw_type(a), raw_type(b)

  if ta == tb and (ta == 'number' or ta == 'string') then
    return a < b
  end

  return tostring(a) < tostring(b)
end

local function shown_key(key)
  if raw_type(key) == 'string' then
    return '["'..key..'"]'
  end

  return '['..tostring(key)..']'
end

function PrintTable(tbl, indent, done)
  indent = indent or 0
  done = done or {}
  done[tbl] = true

  local keys = {}

  for key in pairs(tbl) do
    keys[#keys + 1] = key
  end

  table.sort(keys, key_order)

  local margin = string.rep('\t', indent)

  for _, key in ipairs(keys) do
    local value = tbl[key]

    if raw_type(value) == 'table' and not done[value] then
      Msg(margin..shown_key(key)..':\n')
      PrintTable(value, indent + 2, done)
    else
      Msg(margin..shown_key(key)..'\t=\t'..tostring(value)..'\n')
    end
  end

  done[tbl] = nil
end

FORCE_STRING = 1
FORCE_NUMBER = 2
FORCE_BOOL = 3
FORCE_ANGLE = 4
FORCE_COLOR = 5
FORCE_VECTOR = 6

local forcers = {
  [FORCE_STRING] = tostring,
  [FORCE_NUMBER] = tonumber,
  [FORCE_BOOL] = tobool,
  [FORCE_ANGLE] = function(value) return Angle(value) end,
  [FORCE_COLOR] = function(value)
    if IsColor(value) then
      return value
    end

    return string.ToColor(tostring(value))
  end,
  [FORCE_VECTOR] = function(value)
    if IsColor(value) then
      return value:ToVector()
    end

    return Vector(value)
  end
}

function AccessorFunc(tab, key, name, force)
  local convert = forcers[force]

  tab['Get'..name] = function(self)
    return self[key]
  end

  if convert then
    tab['Set'..name] = function(self, value)
      self[key] = convert(value)
    end
  else
    tab['Set'..name] = function(self, value)
      self[key] = value
    end
  end
end

function RandomPairs(tbl, descending)
  local keys = {}

  for key in pairs(tbl) do
    keys[#keys + 1] = key
  end

  for i = #keys, 2, -1 do
    local j = math.random(i)

    keys[i], keys[j] = keys[j], keys[i]
  end

  local position, step = 0, 1

  if descending then
    position, step = #keys + 1, -1
  end

  return function()
    position = position + step

    local key = keys[position]

    if key ~= nil then
      return key, tbl[key]
    end
  end
end

local function report_failure(message)
  local stack = debug.traceback('', 2):gsub('^\n', '')

  ErrorNoHalt('[ERROR] '..tostring(message)..'\n'..stack..'\n\n')
end

function ProtectedCall(func, ...)
  return (xpcall(func, report_failure, ...))
end

function IncludeCS(path)
  AddCSLuaFile(path)

  return include(path)
end

function Add_NPC_Class(name)
  _G[name] = NUM_AI_CLASSES
  NUM_AI_CLASSES = NUM_AI_CLASSES + 1
end

local function name_set(list)
  local set = {}

  for name in list:gmatch('%S+') do
    set[name] = true
  end

  return set
end

local enemy_classes = name_set([[
  npc_antlion npc_antlionguard npc_antlionguardian npc_antlion_worker npc_barnacle npc_breen npc_clawscanner
  npc_combine_s npc_combine_camera npc_combinedropship npc_combinegunship npc_cscanner
  npc_fastzombie npc_fastzombie_torso npc_headcrab npc_headcrab_black npc_headcrab_fast
  npc_headcrab_poison npc_helicopter npc_hunter npc_manhack npc_metropolice npc_poisonzombie
  npc_stalker npc_strider npc_turret_ceiling npc_turret_floor npc_zombie npc_zombie_torso
  npc_zombine monster_alien_controller monster_alien_grunt monster_alien_slave
  monster_babycrab monster_bigmomma monster_bullchicken monster_cockroach monster_gargantua
  monster_headcrab monster_houndeye monster_human_assassin monster_human_grunt
  monster_miniturret monster_nihilanth monster_sentry monster_tentacle monster_turret
  monster_zombie
]])

local friend_classes = name_set([[
  npc_alyx npc_barney npc_citizen npc_dog npc_eli npc_fisherman npc_gman npc_kleiner
  npc_magnusson npc_monk npc_mossman npc_odessa npc_vortigaunt monster_barney
  monster_scientist
]])

function IsEnemyEntityName(class_name)
  return enemy_classes[class_name] == true
end

function IsFriendEntityName(class_name)
  return friend_classes[class_name] == true
end

function SafeRemoveEntity(ent)
  if IsValid(ent) and not ent:IsPlayer() then
    ent:Remove()
  end
end

function SafeRemoveEntityDelayed(ent, delay)
  if IsValid(ent) and not ent:IsPlayer() then
    timer.Simple(delay, function()
      SafeRemoveEntity(ent)
    end)
  end
end

function IsTableOfEntitiesValid(list)
  if not list then
    return false
  end

  for _, ent in pairs(list) do
    if not IsValid(ent) then
      return false
    end
  end

  return true
end

function debug.Trace()
  local lines = { '', 'Trace:' }
  local level = 2

  while true do
    local info = debug.getinfo(level, 'Sln')

    if not info then
      break
    end

    lines[#lines + 1] = format(
      '%3d: Line %d "%s"\t%s', level - 1, info.currentline or -1, info.name or '?', info.short_src or '?'
    )
    level = level + 1
  end

  MsgN(table.concat(lines, '\n'))
end

local Stack = {}

Stack.__index = Stack

function Stack:Push(value)
  local size = self.size + 1

  self.items[size] = value
  self.size = size
end

function Stack:Top()
  return self.items[self.size]
end

function Stack:Size()
  return self.size
end

local function check_pop(stack, amount)
  if amount < 0 then
    error('attempted to pop '..amount..' elements in stack, expected >= 0', 3)
  elseif amount > stack.size then
    local noun = amount == 1 and ' element' or ' elements'

    error('attempted to pop '..amount..noun..' in stack of length '..stack.size, 3)
  end
end

local function take(stack)
  local size = stack.size
  local value = stack.items[size]

  stack.items[size] = nil
  stack.size = size - 1

  return value
end

function Stack:Pop(amount)
  amount = amount or 1
  check_pop(self, amount)

  local last

  for _ = 1, amount do
    last = take(self)
  end

  return last
end

function Stack:PopMulti(amount)
  amount = amount or 1
  check_pop(self, amount)

  local popped = {}

  for i = 1, amount do
    popped[i] = take(self)
  end

  return popped
end

function util.Stack()
  return setmetatable({ items = {}, size = 0 }, Stack)
end

local Timer = {}

Timer.__index = Timer

function Timer:Start(duration)
  self.begun = CurTime()
  self.finish = self.begun + (duration or 0)
  self.running = true
end

function Timer:Reset()
  self.finish = 0
  self.running = false
end

function Timer:Started()
  return self.running
end

function Timer:Elapsed()
  return CurTime() >= self.finish
end

function Timer:GetElaspedTime()
  return CurTime() - self.begun
end

function util.Timer(duration)
  local timer = setmetatable({}, Timer)

  timer:Start(duration)

  return timer
end

function util.NiceFloat(value)
  local text = format('%f', value):gsub('0+$', ''):gsub('%.$', '')

  return text
end

function util.DateStamp()
  local now = os.date('*t')

  return now.year..'-'..now.month..'-'..now.day..' '..format('%02i-%02i-%02i', now.hour, now.min, now.sec)
end

local function components(...)
  local parts = { ... }

  for i = 1, #parts do
    parts[i] = util.NiceFloat(parts[i])
  end

  return table.concat(parts, ' ')
end

function util.TypeToString(value)
  local kind = type(value)

  if kind == 'number' then
    return util.NiceFloat(value)
  elseif kind == 'boolean' then
    return value and 'true' or 'false'
  elseif kind == 'Vector' then
    return components(value.x, value.y, value.z)
  elseif kind == 'Angle' then
    return components(value.p, value.y, value.r)
  elseif IsColor(value) then
    return components(value.r, value.g, value.b, value.a)
  end

  return tostring(value)
end

local from_string = {
  string = function(text) return text end,
  bool = tobool,
  boolean = tobool,
  float = function(text) return tonumber(text) end,
  number = function(text) return tonumber(text) end,
  int = function(text)
    local n = tonumber(text)

    return n and math.Round(n)
  end,
  vector = function(text) return Vector(text) end,
  angle = function(text) return Angle(text) end,
  entity = function(text) return Entity(tonumber(text) or -1) end
}

function util.StringToType(text, type_name)
  local convert = from_string[string.lower(type_name)]

  if convert then
    return convert(text)
  end
end

local platform_suffix

do
  local os_name = jit and jit.os or ''
  local wide = jit and jit.arch == 'x64'

  if os_name == 'Windows' then
    platform_suffix = wide and 'win64' or 'win32'
  elseif os_name == 'OSX' then
    platform_suffix = wide and 'osx64' or 'osx'
  else
    platform_suffix = wide and 'linux64' or 'linux'
  end
end

function util.IsBinaryModuleInstalled(name)
  if raw_type(name) ~= 'string' then
    error("bad argument #1 to 'IsBinaryModuleInstalled' (string expected, got "..type(name)..')', 2)
  end

  if name == '' then
    error("bad argument #1 to 'IsBinaryModuleInstalled' (string cannot be empty)", 2)
  end

  if name:find('[/\\%.]') then
    return false
  end

  return file.Exists('lua/bin/gmsv_'..name..'_'..platform_suffix..'.dll', 'MOD')
end

local PDATA_SCHEMA = 'CREATE TABLE IF NOT EXISTS playerpdata '
  ..'(infoid TEXT NOT NULL, value TEXT, PRIMARY KEY (infoid))'

local function pdata_run(statement, ...)
  if sql.Query(PDATA_SCHEMA) == false then
    return false
  end

  return sql.QueryTyped(statement, ...)
end

local function pdata_keys(steam_id, name)
  local suffix = '['..tostring(name)..']'

  return util.SteamIDTo64(steam_id)..suffix, util.CRC('gm_'..steam_id..'_gm')..suffix
end

local function stored_pdata(key)
  local rows = pdata_run('SELECT value FROM playerpdata WHERE infoid = ?', key)

  return rows and rows[1] and rows[1].value
end

local PDATA_STORE = 'INSERT INTO playerpdata (infoid, value) VALUES (?, ?) '
  ..'ON CONFLICT (infoid) DO UPDATE SET value = excluded.value'

local function store_pdata(steam_id, name, value)
  local key = pdata_keys(steam_id, name)

  return pdata_run(PDATA_STORE, key, tostring(value)) ~= false
end

local function forget_pdata(steam_id, name)
  local key, legacy_key = pdata_keys(steam_id, name)

  return pdata_run('DELETE FROM playerpdata WHERE infoid = ? OR infoid = ?', key, legacy_key) ~= false
end

function util.GetPData(steam_id, name, default)
  local key, legacy_key = pdata_keys(steam_id, name)
  local value = stored_pdata(key)

  if value == nil then
    value = stored_pdata(legacy_key)
  end

  if value == nil then
    return default
  end

  return value
end

function util.SetPData(steam_id, name, value)
  store_pdata(steam_id, name, value)
end

function util.RemovePData(steam_id, name)
  forget_pdata(steam_id, name)
end

do
  local player_meta = FindMetaTable('Player')

  function player_meta:GetPData(name, default)
    return util.GetPData(self:SteamID(), name, default)
  end

  function player_meta:SetPData(name, value)
    return store_pdata(self:SteamID(), name, value)
  end

  function player_meta:RemovePData(name)
    return forget_pdata(self:SteamID(), name)
  end
end
