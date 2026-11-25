-- fpkg's GLua runtime: the game's additions to the `table` library, and the sorted
-- iterators.
-- Copies and merges (`Copy`, deep, with the real metatables, a table that holds tables copied
-- once and reused wherever it is met again, cycles included, one that holds none copied
-- wherever it is met; `CopyFromTo`, `Merge`, `Inherit`, `Add`, which appends every value of
-- the source and nothing of a table to itself, `Empty`, `Pack`, `ForceInsert`), queries
-- (`Count`, `IsEmpty`, `IsSequential`, `HasValue`, `KeyFromValue`, `KeysFromValue`,
-- `GetKeys`, `GetFirstKey`, `GetFirstValue`, `GetLastKey`, `GetLastValue`, `GetWinningKey`,
-- `FindNext`, `FindPrev`, `Random`, `MemberValuesFromKey`), reshaping (`Flip`, `Reverse`,
-- `ClearKeys`, `CollapseKeyValue`, `LowerKeyNames`, `Sanitise`, `DeSanitise`, `ToString`,
-- which writes colors as `Color(r,g,b,a)` and a table met again inside itself as its
-- address, `RemoveByValue`, `Shuffle`, `ForEach`), sorting (`SortDesc`; `SortByKey`, the
-- keys by value, highest first unless its second argument is true; `SortByMember`,
-- descending unless asked, strings compared without case, missing members last), and
-- `SortedPairs`, `SortedPairsByValue` and `SortedPairsByMemberValue` (ordered as
-- `SortByMember` orders).

local lib = table
local sort, remove, concat = lib.sort, lib.remove, lib.concat
local pairs, ipairs, next, type, tostring = pairs, ipairs, next, type, tostring
local get_meta, set_meta = debug.getmetatable, setmetatable
local random = math.random
local rep, lower = string.rep, string.lower

local function holds_tables(tbl)
  for _, value in pairs(tbl) do
    if type(value) == 'table' then
      return true
    end
  end

  return false
end

local function copy_of(source, copies)
  local done = copies[source]

  if done then
    return done
  end

  local copy = {}

  if holds_tables(source) then
    copies[source] = copy
  end

  set_meta(copy, get_meta(source))

  for key, value in pairs(source) do
    if type(value) == 'table' then
      copy[key] = copy_of(value, copies)
    else
      copy[key] = value
    end
  end

  return copy
end

function lib.Copy(source, copies)
  if source == nil then
    return nil
  end

  return copy_of(source, copies or {})
end

function lib.Merge(target, source, force_override)
  for key, value in pairs(source) do
    local own = target[key]

    if not force_override and type(value) == 'table' and type(own) == 'table' then
      lib.Merge(own, value)
    else
      target[key] = value
    end
  end

  return target
end

function lib.Empty(tbl)
  local key = next(tbl)

  while key ~= nil do
    tbl[key] = nil
    key = next(tbl)
  end
end

function lib.CopyFromTo(source, target)
  lib.Empty(target)
  lib.Merge(target, source)
end

function lib.Inherit(target, base)
  for key, value in pairs(base) do
    if target[key] == nil then
      target[key] = value
    end
  end

  target.BaseClass = base

  return target
end

function lib.Add(target, source)
  if type(source) ~= 'table' or source == target then
    return target
  end

  if type(target) ~= 'table' then
    target = {}
  end

  for _, value in pairs(source) do
    target[#target + 1] = value
  end

  return target
end

function lib.Pack(...)
  return { ... }, select('#', ...)
end

function lib.ForceInsert(tbl, value)
  tbl = tbl or {}
  tbl[#tbl + 1] = value

  return tbl
end

function lib.Count(tbl)
  local count = 0

  for _ in pairs(tbl) do
    count = count + 1
  end

  return count
end

function lib.IsEmpty(tbl)
  return next(tbl) == nil
end

function lib.IsSequential(tbl)
  local count = 0

  for _ in pairs(tbl) do
    count = count + 1

    if tbl[count] == nil then
      return false
    end
  end

  return true
end

function lib.HasValue(tbl, value)
  for _, candidate in pairs(tbl) do
    if candidate == value then
      return true
    end
  end

  return false
end

function lib.KeyFromValue(tbl, value)
  for key, candidate in pairs(tbl) do
    if candidate == value then
      return key
    end
  end
end

function lib.KeysFromValue(tbl, value)
  local keys = {}

  for key, candidate in pairs(tbl) do
    if candidate == value then
      keys[#keys + 1] = key
    end
  end

  return keys
end

function lib.RemoveByValue(tbl, value)
  local key = lib.KeyFromValue(tbl, value)

  if key == nil then
    return false
  end

  if type(key) == 'number' then
    remove(tbl, key)
  else
    tbl[key] = nil
  end

  return key
end

function lib.GetKeys(tbl)
  local keys = {}

  for key in pairs(tbl) do
    keys[#keys + 1] = key
  end

  return keys
end

function lib.GetFirstKey(tbl)
  return (next(tbl))
end

function lib.GetFirstValue(tbl)
  local _, value = next(tbl)

  return value
end

function lib.GetLastKey(tbl)
  local last

  for key in pairs(tbl) do
    last = key
  end

  return last
end

function lib.GetLastValue(tbl)
  local last

  for _, value in pairs(tbl) do
    last = value
  end

  return last
end

function lib.GetWinningKey(tbl)
  local best_key, best_value

  for key, value in pairs(tbl) do
    if best_value == nil or value > best_value then
      best_key, best_value = key, value
    end
  end

  return best_key
end

function lib.FindNext(tbl, value)
  local found = false

  for _, candidate in ipairs(tbl) do
    if found then
      return candidate
    end

    if candidate == value then
      found = true
    end
  end

  return tbl[1]
end

function lib.FindPrev(tbl, value)
  local previous = tbl[#tbl]

  for _, candidate in ipairs(tbl) do
    if candidate == value then
      return previous
    end

    previous = candidate
  end

  return tbl[#tbl]
end

function lib.Random(tbl)
  local keys = lib.GetKeys(tbl)

  if #keys == 0 then
    return nil
  end

  local key = keys[random(#keys)]

  return tbl[key], key
end

function lib.MemberValuesFromKey(tbl, member)
  local values = {}

  for _, entry in pairs(tbl) do
    values[#values + 1] = entry[member]
  end

  return values
end

function lib.Flip(tbl)
  local flipped = {}

  for key, value in pairs(tbl) do
    flipped[value] = key
  end

  return flipped
end

function lib.Reverse(tbl)
  local reversed = {}
  local count = #tbl

  for i = count, 1, -1 do
    reversed[count - i + 1] = tbl[i]
  end

  return reversed
end

function lib.ClearKeys(tbl, keep_keys)
  local values = {}

  for key, value in pairs(tbl) do
    if keep_keys and type(value) == 'table' then
      value.__key = key
    end

    values[#values + 1] = value
  end

  return values
end

function lib.CollapseKeyValue(entries)
  local collapsed = {}

  for _, entry in ipairs(entries) do
    local value = entry.Value

    if type(value) == 'table' then
      value = lib.CollapseKeyValue(value)
    end

    collapsed[entry.Key] = value
  end

  return collapsed
end

function lib.LowerKeyNames(tbl)
  local lowered = {}

  for key, value in pairs(tbl) do
    if type(key) == 'string' then
      key = lower(key)
    end

    if type(value) == 'table' then
      value = lib.LowerKeyNames(value)
    end

    lowered[key] = value
  end

  return lowered
end

local function sanitized(value)
  local kind = type(value)

  if kind == 'boolean' then
    return { __type = 'Bool', tostring(value) }
  elseif kind == 'Vector' then
    return { __type = 'Vector', x = value.x, y = value.y, z = value.z }
  elseif kind == 'Angle' then
    return { __type = 'Angle', p = value.p, y = value.y, r = value.r }
  elseif IsColor(value) then
    local sanitized_color = { __type = 'Color' }

    for _, channel in ipairs({ 'r', 'g', 'b', 'a' }) do
      if value[channel] ~= 255 then
        sanitized_color[channel] = value[channel]
      end
    end

    return sanitized_color
  elseif kind == 'table' then
    return lib.Sanitise(value)
  elseif kind == 'number' then
    return tostring(value)
  end

  return value
end

function lib.Sanitise(tbl)
  local out = {}

  for key, value in pairs(tbl) do
    out[key] = sanitized(value)
  end

  return out
end

local function restored(value)
  if type(value) ~= 'table' then
    return value
  end

  local kind = value.__type

  if kind == 'Bool' then
    return value[1] == 'true'
  elseif kind == 'Vector' then
    return Vector(value.x, value.y, value.z)
  elseif kind == 'Angle' then
    return Angle(value.p, value.y, value.r)
  elseif kind == 'Color' then
    return Color(value.r or 255, value.g or 255, value.b or 255, value.a or 255)
  end

  return lib.DeSanitise(value)
end

function lib.DeSanitise(tbl)
  local out = {}

  for key, value in pairs(tbl) do
    out[key] = restored(value)
  end

  return out
end

function lib.Shuffle(tbl)
  for i = #tbl, 2, -1 do
    local j = random(i)

    tbl[i], tbl[j] = tbl[j], tbl[i]
  end
end

function lib.ForEach(tbl, func)
  for key, value in pairs(tbl) do
    func(key, value)
  end
end

local function key_text(key)
  local kind = type(key)

  if kind == 'number' or kind == 'boolean' then
    return '['..tostring(key)..']'
  end

  return tostring(key)
end

local function value_text(value)
  if type(value) == 'string' then
    return '"'..value..'"'
  elseif IsColor(value) then
    return 'Color('..value.r..','..value.g..','..value.b..','..value.a..')'
  end

  return tostring(value)
end

local function write_entries(tbl, depth, tab, newline, out, entered)
  local indent = rep(tab, depth + 2)
  local listed = lib.IsSequential(tbl)
  local iterate = listed and ipairs or pairs

  for key, value in iterate(tbl) do
    local head = indent..tab

    if not listed then
      head = indent..key_text(key)..tab..'='..tab
    end

    if type(value) == 'table' and not IsColor(value) and not entered[value] then
      entered[value] = true
      out[#out + 1] = head..'{'..newline
      write_entries(value, depth + 1, tab, newline, out, entered)
      out[#out + 1] = rep(tab, depth + 4)..'},'..newline
    else
      out[#out + 1] = head..value_text(value)..','..newline
    end
  end
end

function lib.ToString(tbl, name, nice)
  local tab, newline = '', ''

  if nice then
    tab, newline = '\t', '\n'
  end

  local out = {}

  if name then
    out[1] = name..tab..'='..tab
  end

  out[#out + 1] = '{'..newline
  write_entries(tbl, 0, tab, newline, out, {})
  out[#out + 1] = '}'

  return concat(out)
end

function lib.SortDesc(tbl)
  sort(tbl, function(a, b)
    return a > b
  end)
end

function lib.SortByKey(tbl, lowest_first)
  local keys = lib.GetKeys(tbl)

  if lowest_first then
    sort(keys, function(a, b) return tbl[a] < tbl[b] end)
  else
    sort(keys, function(a, b) return tbl[a] > tbl[b] end)
  end

  return keys
end

local function member_values(a, b, member)
  local x, y = a[member], b[member]

  if type(x) == 'string' then
    return x:lower(), y:lower()
  end

  return x, y
end

local function member_order(member, ascending)
  if ascending then
    return function(a, b)
      if not a[member] then
        return false
      elseif not b[member] then
        return true
      end

      local x, y = member_values(a, b, member)

      return x < y
    end
  end

  return function(a, b)
    if not a[member] then
      return true
    elseif not b[member] then
      return false
    end

    local x, y = member_values(a, b, member)

    return x > y
  end
end

function lib.SortByMember(tbl, member, ascending)
  sort(tbl, member_order(member, ascending))
end

local function ordered_pairs(tbl, keys)
  local position = 0

  return function()
    position = position + 1

    local key = keys[position]

    if key ~= nil then
      return key, tbl[key]
    end
  end
end

local function ascending(a, b)
  return a < b
end

local function descending_order(a, b)
  return a > b
end

function SortedPairs(tbl, descending)
  local keys = lib.GetKeys(tbl)

  sort(keys, descending and descending_order or ascending)

  return ordered_pairs(tbl, keys)
end

function SortedPairsByValue(tbl, descending)
  local keys = lib.GetKeys(tbl)

  if descending then
    sort(keys, function(a, b) return tbl[a] > tbl[b] end)
  else
    sort(keys, function(a, b) return tbl[a] < tbl[b] end)
  end

  return ordered_pairs(tbl, keys)
end

function SortedPairsByMemberValue(tbl, member, descending)
  local keys = lib.GetKeys(tbl)
  local order = member_order(member, not descending)

  sort(keys, function(a, b)
    return order(tbl[a], tbl[b])
  end)

  return ordered_pairs(tbl, keys)
end
