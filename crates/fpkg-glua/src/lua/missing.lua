-- fpkg's GLua runtime: the loud failure of the server API it does not implement.
-- Every library of the server API (api.lua) exists; a function of it that the runtime
-- leaves out, and a global function of the API that does not exist, is a function that
-- raises "fpkg's runtime does not implement <name>" when it is called. Other missing names
-- stay nil, as on the server.

local api = ...

local function failing(name)
  return function()
    error("fpkg's runtime does not implement "..name, 2)
  end
end

local function names_of(list)
  local set = {}

  for name in list:gmatch('%S+') do
    set[name] = true
  end

  return set
end

local function library(path)
  local t = _G

  for part in path:gmatch('[^.]+') do
    local v = rawget(t, part)

    if type(v) ~= 'table' then
      v = {}
      rawset(t, part, v)
    end

    t = v
  end

  return t
end

local function guard(t, prefix, names)
  local meta = getmetatable(t)

  if meta == nil then
    meta = {}
    setmetatable(t, meta)
  elseif meta.__index ~= nil then
    return
  end

  local made = {}

  meta.__index = function(_, key)
    if names[key] then
      made[key] = made[key] or failing(prefix..tostring(key))

      return made[key]
    end
  end
end

for path, list in pairs(api) do
  if path == '_G' then
    guard(_G, '', names_of(list))
  else
    guard(library(path), path..'.', names_of(list))
  end
end
