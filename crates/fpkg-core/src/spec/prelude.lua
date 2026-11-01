-- The trusted half of the spec sandbox, run before the sandbox is switched on:
-- fills `env` with the stubs a spec may call and returns what the host reads back.
local env, host = ...

local error, type, tostring, tonumber, setmetatable, getmetatable, rawget =
  error, type, tostring, tonumber, setmetatable, getmetatable, rawget
-- The state's own loader (not the sandbox's, which puts back the names
-- removed below on every call); env.load wraps it.
local raw_load = load
local MAX_STRING = 16 * 1024 * 1024

-- The count hook that enforces the instruction budget does not run inside
-- JIT-compiled traces, so specs are interpreted only.
local jit_lib = jit or (package and package.loaded and package.loaded.jit)
if not jit_lib then error('fpkg: the jit library is not available to turn the JIT off') end
jit_lib.off()
jit_lib.flush()

local meta = {}        -- Package:describe fields, plural names stored singular
local deps = {}        -- { name, constraint } in call order
local rec = { resources = {}, client_env = {}, cs_files = {}, includes = {}, output = {} }
local state = { meta = meta, deps = deps, rec = rec, described = false }
local spec_tables = setmetatable({}, { __mode = 'k' })

local function push(t, v) t[#t + 1] = v end

-- PackageInstance:depends(what), which Flux calls as s.depends 'name' (self is
-- the name) and as s:depends('name'). fpkg also takes a constraint as the next
-- argument: s.depends('sfs', '~> 7.0').
local function depends(a, b, c)
  local name, constraint = a, b
  if type(a) == 'table' and (spec_tables[a] or a == rawget(env, 'PACKAGE')) then
    name, constraint = b, c
  end
  if type(name) ~= 'string' then
    error('depends expects a package id or a .lua path, got ' .. type(name), 2)
  end
  if constraint ~= nil and type(constraint) ~= 'string' then
    error('the constraint of depends must be a string, got ' .. type(constraint), 2)
  end
  push(deps, { name, constraint })
end

local PACKAGE = { __path__ = host.path, metadata = meta, depends = depends }

local Package = {}

-- Package:describe(callback), exactly as lib/package.lua maps field names: a
-- name ending in 'ies' ends in 'y' instead, any other name ending in 's' loses
-- it (authors -> author, files -> file, dependencies -> dependency).
function Package.describe(self, callback)
  if callback == nil and type(self) == 'function' then callback = self end
  if callback ~= nil then
    if type(callback) ~= 'function' then
      error('Package:describe expects a function', 2)
    end
    local s = setmetatable({ depends = depends }, { __newindex = function(_, k, v)
      if type(k) ~= 'string' then
        error('field names of the spec must be strings, got ' .. type(k), 2)
      end
      if k:sub(-1) == 's' then
        if k:sub(-3) == 'ies' then
          k = k:gsub('ies$', 'y')
        else
          k = k:gsub('s$', '')
        end
      end
      meta[k] = v
    end })
    spec_tables[s] = true
    state.described = true
    callback(s)
  end
  return PACKAGE
end

local function record(list, what)
  return function(value)
    if type(value) ~= 'string' and value ~= nil then
      error(what .. ' expects a string, got ' .. type(value), 2)
    end
    if value ~= nil then push(list, value) end
  end
end

local function refuse(name)
  return function()
    error(name .. ' is not available in packagespec.lua', 2)
  end
end

env.Package = Package
env.PACKAGE = PACKAGE
env.add_client_env = function(name, value)
  if type(name) ~= 'string' then
    error('add_client_env expects a name, got ' .. type(name), 2)
  end
  push(rec.client_env, { name, tostring(value) })
end
env.resource = {
  AddSingleFile = record(rec.resources, 'resource.AddSingleFile'),
  AddFile = record(rec.resources, 'resource.AddFile'),
  AddWorkshop = function() end,
}
env.AddCSLuaFile = record(rec.cs_files, 'AddCSLuaFile')
env.include = record(rec.includes, 'include')
env.require_relative = record(rec.includes, 'require_relative')
env.file = {
  Find = function(pattern, where)
    if type(pattern) ~= 'string' then
      error('file.Find expects a pattern, got ' .. type(pattern), 2)
    end
    return host.find(pattern, where or 'GAME')
  end,
  Exists = function(path, where)
    if type(path) ~= 'string' then
      error('file.Exists expects a path, got ' .. type(path), 2)
    end
    return host.exists(path, where or 'GAME')
  end,
}
env.SERVER = true
env.CLIENT = false
env.IS_DEVELOPMENT = host.env == 'development'
env.IS_TEST = host.env == 'test'
env.IS_PRODUCTION = host.env == 'production'
env.engine = { ActiveGamemode = function() return host.project_id end }
env.istable = function(v) return type(v) == 'table' end
env.isstring = function(v) return type(v) == 'string' end
env.isfunction = function(v) return type(v) == 'function' end
env.isnumber = function(v) return type(v) == 'number' end
env.isbool = function(v) return type(v) == 'boolean' end
env.tobool = function(v)
  return not (v == nil or v == false or v == 0 or v == '0' or v == 'false')
end
-- What the sandbox has that would reach outside the spec: printing (it would
-- corrupt --json output, so it is recorded), the file searchers of require and
-- add_path.
env.print = function(...)
  local n, parts = select('#', ...), {}
  for i = 1, n do parts[i] = tostring((select(i, ...))) end
  push(rec.output, table.concat(parts, '\t'))
end
env.require = refuse('require')

-- Names of the nl sandbox that a spec has no use for and that reach shared
-- state: the module system, NL's loader and event bus, keyed arguments, raise.
env.package = nil
env.load_nl = nil
env.event = nil
env.keyed = nil
env.raise = nil
env.add_path = nil

-- load and loadstring take source text only, in this environment.
env.load = function(chunk, name, mode, chunk_env)
  if type(chunk) ~= 'string' then
    error('load takes source text in packagespec.lua', 2)
  end
  if chunk:byte(1) == 27 then
    return nil, 'bytecode loading is off in packagespec.lua'
  end
  return raw_load(chunk, name, 't', chunk_env or env)
end
env.loadstring = function(chunk, name)
  return env.load(chunk, name)
end

-- Strings index the sandbox's copy of the string library (no dump), and
-- string.rep cannot build a string larger than 16 MiB in one call.
local str = env.string
local rep = str.rep
str.rep = function(s, n, sep)
  s, n = tostring(s), tonumber(n) or 0
  sep = sep ~= nil and tostring(sep) or ''
  if n > 0 and (#s + #sep) * n > MAX_STRING then
    error('string.rep would build a string larger than 16 MiB', 2)
  end
  return rep(s, n, sep)
end
getmetatable('').__index = str

-- A spec that reaches an unknown global gets an error naming it and the line
-- (level 2 is the code that read the global), not a silent nil.
setmetatable(env, { __index = function(_, k)
  error("unknown global '" .. tostring(k) .. "'", 2)
end })

return state
