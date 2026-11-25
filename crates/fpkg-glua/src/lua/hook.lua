-- fpkg's GLua runtime: the `hook` library.
-- `hook.Add(event, identifier, func)` registers a function under a string identifier, or
-- under an object with an `IsValid` method, which the function then receives first and
-- which is removed as soon as it is no longer valid; adding an identifier again replaces
-- its function. Arguments of the wrong type are reported (`ErrorNoHaltWithStack`) and
-- ignored; `hook.Remove` without an identifier raises an error. `hook.Call(event, gamemode,
-- ...)` calls the functions of the event in the order they were added until one returns a
-- value other than nil, and returns the first six values it returned; when none does, it
-- returns what `gamemode[event](gamemode, ...)` returns (a false gamemode is none).
-- `hook.GetTable()` gives the functions by event and identifier, and may be edited: what it
-- holds at a call is what is called (functions put there directly come after the others).
-- While an event is being called, a function removed from it is not called any more, an
-- identifier registered when the call began runs its current function, and identifiers new
-- to the event run from its next call. `hook.Run(event, ...)` is `hook.Call` with the current
-- gamemode.

local type, unpack, pairs, ipairs = type, unpack, pairs, ipairs

hook = hook or {}

local functions_of = {}
local order_of = {}
local empty_order = { list = {}, listed = {} }

local function report(position, func_name, expected, value)
  ErrorNoHaltWithStack(
    'bad argument #'..position.." to '"..func_name.."' ("..expected..' expected, got '..type(value)..')'
  )
end

local function order_with(order, identifier)
  local list, listed = {}, {}

  for i, known in ipairs(order.list) do
    list[i] = known
    listed[known] = true
  end

  list[#list + 1] = identifier
  listed[identifier] = true

  return { list = list, listed = listed }
end

local function order_keeping(order, keep)
  local list, listed = {}, {}

  for _, known in ipairs(order.list) do
    if keep(known) then
      list[#list + 1] = known
      listed[known] = true
    end
  end

  return { list = list, listed = listed }
end

local function current_order(event, functions)
  local order = order_of[event] or empty_order

  for _, known in ipairs(order.list) do
    if functions[known] == nil then
      order = order_keeping(order, function(identifier) return functions[identifier] ~= nil end)

      break
    end
  end

  for identifier in pairs(functions) do
    if not order.listed[identifier] then
      order = order_with(order, identifier)
    end
  end

  order_of[event] = order

  return order
end

local plain_kinds = { ['nil'] = true, number = true, boolean = true, ['function'] = true, thread = true }

local function is_object(identifier)
  return not plain_kinds[type(identifier)] and type(identifier.IsValid) == 'function'
end

function hook.Add(event, identifier, func)
  if type(event) ~= 'string' then
    return report(1, 'Add', 'string', event)
  elseif type(identifier) ~= 'string' and not is_object(identifier) then
    return report(2, 'Add', 'string', identifier)
  elseif type(func) ~= 'function' then
    return report(3, 'Add', 'function', func)
  end

  local functions = functions_of[event]

  if functions == nil then
    functions = {}
    functions_of[event] = functions
  end

  local order = current_order(event, functions)

  if not order.listed[identifier] then
    order_of[event] = order_with(order, identifier)
  end

  functions[identifier] = func
end

function hook.Remove(event, identifier)
  if type(event) ~= 'string' then
    return report(1, 'Remove', 'string', event)
  elseif identifier == nil then
    error('attempt to index a nil value')
  elseif type(identifier) == 'number' or type(identifier) == 'boolean' then
    return report(2, 'Remove', 'string', identifier)
  end

  local functions = functions_of[event]

  if functions == nil or functions[identifier] == nil then
    return
  end

  functions[identifier] = nil
  order_of[event] = order_keeping(current_order(event, functions), function(known)
    return known ~= identifier
  end)
end

function hook.GetTable()
  return functions_of
end

local function answered(...)
  if (...) == nil then
    return nil
  end

  return { ... }
end

function hook.Call(event, gamemode, ...)
  local functions = functions_of[event]

  if functions ~= nil then
    local list = current_order(event, functions).list

    for i = 1, #list do
      local identifier = list[i]
      local func = functions[identifier]

      if func ~= nil then
        local results

        if type(identifier) == 'string' then
          results = answered(func(...))
        elseif IsValid(identifier) then
          results = answered(func(identifier, ...))
        else
          hook.Remove(event, identifier)
        end

        if results then
          return unpack(results, 1, 6)
        end
      end
    end
  end

  if not gamemode then
    return
  end

  local method = gamemode[event]

  if method == nil then
    return
  end

  return method(gamemode, ...)
end

function hook.Run(event, ...)
  return hook.Call(event, gmod.GetGamemode(), ...)
end
