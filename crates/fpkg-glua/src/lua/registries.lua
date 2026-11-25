-- fpkg's GLua runtime: the registries of classes, `baseclass`, `weapons` and
-- `scripted_ents`.
-- `baseclass.Get(name)` gives the table of a class, an empty one when the class is not known
-- yet that `baseclass.Set(name, tab)` fills later (it copies the fields and the metatable of
-- `tab`, and names the class in `ThisClass`), so that a base can be taken before it is
-- registered; it also sets the `Base` of the `ENT` and `SWEP` being defined, if any.
-- `weapons.Register` and `scripted_ents.Register` keep a class, under its
-- `ClassNameOverride` if it has one (unless the `PreRegisterSWEP` or `PreRegisterSENT` hook
-- returns false), and set its `ClassName`; `Get` returns a copy of a class with what it
-- inherits from its chain of bases (sub-tables, colors kept as they are, are copied with their
-- metatables, merged and given a `BaseClass`; the copy's `BaseClass` is the copy of its
-- base), filling the table given if any; a weapon without a `Base` is based on `weapon_base`.
-- `GetStored`, `GetList`, `IsBasedOn`, and `OnLoaded`, which makes base classes of the
-- registered classes (the engine calls it once they are all loaded). Scripted entities are
-- stored as `{ t = ENT, Base = ..., type = ENT.Type, isBaseType = true }`, their base
-- defaulting by type (`base_anim`, `base_point`, `base_brush`, `base_filter`, which
-- `GetType` knows), and also have `Alias` (names that `Get` accepts), `GetType`, `GetMember`
-- and `GetSpawnable`.

local type, pairs = type, pairs
local get_meta, set_meta = getmetatable, setmetatable

baseclass = baseclass or {}
weapons = weapons or {}
scripted_ents = scripted_ents or {}

local classes = {}

function baseclass.Get(name)
  if ENT then
    ENT.Base = name
  end

  if SWEP then
    SWEP.Base = name
  end

  local class = classes[name]

  if class == nil then
    class = {}
    classes[name] = class
  end

  return class
end

function baseclass.Set(name, tab)
  tab.ThisClass = name

  local class = classes[name]

  if class == nil then
    classes[name] = tab
  elseif class ~= tab then
    for key, value in pairs(tab) do
      class[key] = value
    end

    set_meta(class, get_meta(tab))
  end
end

local function member_copy(source, copies)
  local existing = copies[source]

  if existing then
    return existing
  end

  local copy = set_meta({}, debug.getmetatable(source))

  copies[source] = copy

  for key, value in pairs(source) do
    if type(value) == 'table' and not IsColor(value) then
      copy[key] = member_copy(value, copies)
    else
      copy[key] = value
    end
  end

  return copy
end

local function inherit_into(target, base)
  for key, value in pairs(base) do
    local own = target[key]

    if own == nil then
      target[key] = value
    elseif type(own) == 'table' and type(value) == 'table' and key ~= 'BaseClass' then
      inherit_into(own, value)
      own.BaseClass = value
    end
  end
end

local function copier(table_of, base_of, names_base)
  local function copy_class(class, into, seen)
    local source = table_of(class)

    if source == nil or seen[class] then
      return nil
    end

    seen[class] = true

    local result = into or {}
    local copies = {}

    for key, value in pairs(source) do
      if type(value) == 'table' and not IsColor(value) then
        result[key] = member_copy(value, copies)
      else
        result[key] = value
      end
    end

    local base_name = base_of(class, source)

    if names_base and result.Base == nil then
      result.Base = base_name
    end

    if base_name ~= nil and base_name ~= class then
      local base = copy_class(base_name, nil, seen)

      if base then
        inherit_into(result, base)
        result.BaseClass = base
      end
    end

    return result
  end

  return function(class, into)
    return copy_class(class, into, {})
  end
end

local function chain_test(base_of)
  return function(name, base)
    local seen = {}
    local current = base_of(name)

    while current ~= nil and not seen[current] do
      if current == base then
        return true
      end

      seen[current] = true
      current = base_of(current)
    end

    return false
  end
end

local function check_class(class)
  if type(class) ~= 'string' then
    error("bad argument #2 to 'Register' (string expected, got "..type(class)..')', 3)
  end
end

local stored_weapons = {}

local function weapon_table(class)
  return stored_weapons[class]
end

local function weapon_base(_, swep)
  return swep.Base or 'weapon_base'
end

function weapons.Register(swep, class)
  check_class(class)

  if hook.Run('PreRegisterSWEP', swep, class) == false then
    return
  end

  class = swep.ClassNameOverride or class
  swep.ClassName = class
  stored_weapons[class] = swep
end

weapons.Get = copier(weapon_table, weapon_base, true)

function weapons.GetStored(class)
  return stored_weapons[class]
end

function weapons.GetList()
  local list = {}

  for _, swep in pairs(stored_weapons) do
    list[#list + 1] = swep
  end

  return list
end

weapons.IsBasedOn = chain_test(function(class)
  local swep = stored_weapons[class]

  return swep and swep.Base
end)

function weapons.OnLoaded()
  for class in pairs(stored_weapons) do
    baseclass.Set(class, weapons.Get(class))
  end
end

local stored_entities = {}
local aliases = {}

local default_entity_bases = {
  anim = 'base_anim', point = 'base_point', brush = 'base_brush', filter = 'base_filter'
}
local types_of_bases = {}

for kind, base in pairs(default_entity_bases) do
  types_of_bases[base] = kind
end

local function entity_base(class)
  local entry = stored_entities[class]

  return entry and entry.Base
end

function scripted_ents.Register(ent, class)
  check_class(class)

  if hook.Run('PreRegisterSENT', ent, class) == false then
    return
  end

  class = ent.ClassNameOverride or class
  ent.ClassName = class
  stored_entities[class] = {
    t = ent,
    Base = ent.Base or default_entity_bases[ent.Type],
    type = ent.Type,
    isBaseType = true
  }
end

do
  local copy_entity = copier(function(class)
    local entry = stored_entities[class]

    return entry and entry.t
  end, entity_base)

  function scripted_ents.Get(class, into)
    return copy_entity(aliases[class] or class, into)
  end
end

function scripted_ents.GetStored(class)
  return stored_entities[class]
end

function scripted_ents.GetList()
  local list = {}

  for class, entry in pairs(stored_entities) do
    list[class] = entry
  end

  return list
end

function scripted_ents.Alias(alias, class)
  aliases[alias] = class
end

local function inherited_member(class, name)
  local seen = {}

  while class ~= nil and not seen[class] do
    local entry = stored_entities[class]

    if entry == nil then
      return nil
    end

    local value = entry.t[name]

    if value ~= nil then
      return value
    end

    seen[class] = true
    class = entry.Base
  end

  return nil
end

function scripted_ents.GetMember(class, name)
  if stored_entities[class] ~= nil then
    return inherited_member(class, name)
  end
end

function scripted_ents.GetType(class)
  local seen = {}

  while class ~= nil and not seen[class] do
    local entry = stored_entities[class]

    if entry == nil then
      return types_of_bases[class]
    elseif entry.t.Type ~= nil then
      return entry.t.Type
    end

    seen[class] = true
    class = entry.Base
  end

  return nil
end

function scripted_ents.GetSpawnable()
  local list = {}

  for _, entry in pairs(stored_entities) do
    if entry.t.Spawnable then
      list[#list + 1] = entry.t
    end
  end

  return list
end

scripted_ents.IsBasedOn = chain_test(entity_base)

function scripted_ents.OnLoaded()
  for class in pairs(stored_entities) do
    baseclass.Set(class, scripted_ents.Get(class))
  end
end
