-- fpkg's GLua runtime: the base environment of a Garry's Mod server state.
-- The realm constants; the libraries the game takes out of Lua's (file and string
-- loading, `io`, the `os` functions that reach the system) and the Lua 5.0 names it keeps
-- (`string.gfind`, `math.mod`); the `TYPE_`, `FCVAR_` and `FSASYNC_` enumerations;
-- `FindMetaTable` and `RegisterMetaTable` with one persistent table per class name, the
-- classes of the engine registered with their `MetaName` and `MetaID`; `NULL`, the entity
-- that is never valid; and `debug.getregistry`, which gives the metatables by name.

local host = ...

SERVER = true
CLIENT = false
MENU_DLL = false

for _, name in ipairs({ 'dofile', 'load', 'loadfile', 'loadstring', 'io' }) do
  _G[name] = nil
end

package.loaded.io = nil

for _, name in ipairs({ 'execute', 'exit', 'getenv', 'remove', 'rename', 'setlocale', 'tmpname' }) do
  os[name] = nil
end

for _, name in ipairs({ 'cpath', 'path', 'loadlib', 'searchpath' }) do
  package[name] = nil
end

package.loaders = {}

if jit then
  jit.security = nil
end

string.gfind = string.gmatch
math.mod = math.fmod

local types = {
  'NIL', 'BOOL', 'LIGHTUSERDATA', 'NUMBER', 'STRING', 'TABLE', 'FUNCTION', 'USERDATA', 'THREAD',
  'ENTITY', 'VECTOR', 'ANGLE', 'PHYSOBJ', 'SAVE', 'RESTORE', 'DAMAGEINFO', 'EFFECTDATA',
  'MOVEDATA', 'RECIPIENTFILTER', 'USERCMD', 'SCRIPTEDVEHICLE', 'MATERIAL', 'PANEL', 'PARTICLE',
  'PARTICLEEMITTER', 'TEXTURE', 'USERMSG', 'CONVAR', 'IMESH', 'MATRIX', 'SOUND',
  'PIXELVISHANDLE', 'DLIGHT', 'VIDEO', 'FILE', 'LOCOMOTION', 'PATH', 'NAVAREA', 'SOUNDHANDLE',
  'NAVLADDER', 'PARTICLESYSTEM', 'PROJECTEDTEXTURE', 'PHYSCOLLIDE', 'SURFACEINFO'
}

TYPE_NONE = -1

for index, name in ipairs(types) do
  _G['TYPE_'..name] = index - 1
end

TYPE_COUNT = #types
TYPE_COLOR = 255

local fcvar = {
  NONE = 0, UNREGISTERED = 1, DEVELOPMENTONLY = 2, GAMEDLL = 4, CLIENTDLL = 8, HIDDEN = 16,
  PROTECTED = 32, SPONLY = 64, ARCHIVE = 128, NOTIFY = 256, USERINFO = 512,
  PRINTABLEONLY = 1024, UNLOGGED = 2048, NEVER_AS_STRING = 4096, REPLICATED = 8192,
  CHEAT = 16384, DEMO = 65536, DONTRECORD = 131072, LUA_CLIENT = 262144,
  LUA_SERVER = 524288, NOT_CONNECTED = 4194304, ARCHIVE_XBOX = 16777216,
  SERVER_CAN_EXECUTE = 268435456, SERVER_CANNOT_QUERY = 536870912,
  CLIENTCMD_CAN_EXECUTE = 1073741824
}

for name, value in pairs(fcvar) do
  _G['FCVAR_'..name] = value
end

local fsasync = {
  ERR_NOT_MINE = -8, ERR_RETRY_LATER = -7, ERR_ALIGNMENT = -6, ERR_FAILURE = -5,
  ERR_READING = -4, ERR_NOMEMORY = -3, ERR_UNKNOWNID = -2, ERR_FILEOPEN = -1, OK = 0,
  STATUS_PENDING = 1, STATUS_INPROGRESS = 2, STATUS_ABORTED = 3, STATUS_UNSERVICED = 4
}

for name, value in pairs(fsasync) do
  _G['FSASYNC_'..name] = value
end

local metatables = {}
local next_id = 256

function RegisterMetaTable(name, tbl)
  if type(name) ~= 'string' then
    error("bad argument #1 to 'RegisterMetaTable' (string expected, got "..type(name)..')', 2)
  end

  if type(tbl) ~= 'table' then
    error("bad argument #2 to 'RegisterMetaTable' (table expected, got "..type(tbl)..')', 2)
  end

  if metatables[name] == nil then
    if tbl.MetaName == nil then
      tbl.MetaName = name
    end

    if tbl.MetaID == nil then
      tbl.MetaID = next_id
      next_id = next_id + 1
    end

    metatables[name] = tbl
  end
end

function FindMetaTable(name)
  return metatables[name]
end

local classes = {
  { 'Entity', TYPE_ENTITY }, { 'Player', TYPE_ENTITY, 'Entity' }, { 'Weapon', TYPE_ENTITY, 'Entity' },
  { 'NPC', TYPE_ENTITY, 'Entity' }, { 'Vehicle', TYPE_ENTITY, 'Entity' },
  { 'NextBot', TYPE_ENTITY, 'Entity' }, { 'CSEnt', TYPE_ENTITY, 'Entity' },
  { 'Vector', TYPE_VECTOR }, { 'Angle', TYPE_ANGLE }, { 'VMatrix', TYPE_MATRIX },
  { 'PhysObj', TYPE_PHYSOBJ }, { 'ISave', TYPE_SAVE }, { 'IRestore', TYPE_RESTORE },
  { 'CTakeDamageInfo', TYPE_DAMAGEINFO }, { 'CEffectData', TYPE_EFFECTDATA },
  { 'CMoveData', TYPE_MOVEDATA }, { 'CRecipientFilter', TYPE_RECIPIENTFILTER },
  { 'CUserCmd', TYPE_USERCMD }, { 'IMaterial', TYPE_MATERIAL }, { 'ITexture', TYPE_TEXTURE },
  { 'bf_read', TYPE_USERMSG }, { 'ConVar', TYPE_CONVAR }, { 'IMesh', TYPE_IMESH },
  { 'CSoundPatch', TYPE_SOUND }, { 'File', TYPE_FILE }, { 'CLuaLocomotion', TYPE_LOCOMOTION },
  { 'PathFollower', TYPE_PATH }, { 'CNavArea', TYPE_NAVAREA }, { 'CNavLadder', TYPE_NAVLADDER },
  { 'ProjectedTexture', TYPE_PROJECTEDTEXTURE }, { 'PhysCollide', TYPE_PHYSCOLLIDE },
  { 'SurfaceInfo', TYPE_SURFACEINFO }
}

for _, class in ipairs(classes) do
  local meta = { MetaName = class[1], MetaID = class[2] }

  meta.__index = meta

  if class[3] then
    meta.MetaBaseClass = metatables[class[3]]
  end

  metatables[class[1]] = meta
end

do
  local entity = metatables.Entity
  local null_meta = {
    MetaName = 'Entity',
    MetaID = TYPE_ENTITY,
    __index = function(_, key)
      if key == 'IsValid' then
        return function() return false end
      elseif key == 'EntIndex' then
        return function() return 0 end
      end

      local value = entity[key]

      if type(value) == 'function' then
        return function()
          error('Tried to use a NULL entity!', 2)
        end
      end

      return value
    end,
    __tostring = function() return '[NULL Entity]' end,
    __eq = function(a, b) return rawequal(a, b) end
  }

  NULL = newproxy(true)

  local mt = getmetatable(NULL)

  for k, v in pairs(null_meta) do
    mt[k] = v
  end
end

do
  local registry = setmetatable({}, {
    __index = function(_, key)
      return FindMetaTable(key)
    end,
    __newindex = function(self, key, value)
      rawset(self, key, value)

      if type(key) == 'string' and type(value) == 'table' then
        RegisterMetaTable(key, value)
      end
    end
  })

  function debug.getregistry()
    return registry
  end
end

host.metatables = metatables
