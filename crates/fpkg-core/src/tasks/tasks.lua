-- fpkg's task files: the vocabulary of taskspec.lua and the running of tasks, after Rake.
-- `namespace(name, fn)` prefixes the tasks defined inside with `name:`; `desc(text)`
-- describes the next task (`desc(nil)` forgets a description); `task(name,
-- [prerequisites], [fn])` defines a task, or adds prerequisites and an action to one defined
-- before (actions run in the order they were defined). Prerequisites are full task names, or
-- relative to the namespace when they start with `:`; a single one may be given as a
-- string. A name has no blanks, neither starts nor ends with `:`, does not start with `-`
-- and holds no `=`, so that it never reads as a flag or an option. `Tasks.invoke(name,
-- args)` runs a task and its prerequisites, each at most once per fpkg invocation; a name
-- that starts with `:` is relative to the namespace of the task that is running. A task
-- that is reached again while it runs is a cycle, an error that names it. `Tasks.exists(name)`
-- tells whether a task is defined. An action receives `args`: `options` (the KEY=VALUE
-- words), `words` (the others) and `name`.
-- Which namespaces are open is read from the stack (the frames of `namespace`). A running
-- task is kept with the coroutine it runs in, and counts as running only while its frame is
-- on the stack of that coroutine and the coroutine is the running one or resumed the running
-- one, so an error that a `pcall` catches, or a coroutine left suspended in the middle of a
-- task, leaves nothing behind, and a task invoked again from a coroutine is still a cycle.
-- Gets the main thread; returns the table the host drives the tasks with.

local main = ...
local getinfo, getlocal = debug.getinfo, debug.getlocal
local running_thread, status = coroutine.running, coroutine.status

local tasks = {}
local order = {}
local pending_description = nil
local current_package = nil
local done = {}

--- The values of the local `wanted` in the frames of `func` on the stack of `thread`,
-- outermost first.
-- @param thread [Thread whose stack to read]
-- @param func [Function whose frames to look for]
-- @param wanted [String name of the local]
-- @return [Table list of values]
local function frames_of(thread, func, wanted)
  local found = {}
  local level = 0

  while true do
    local info = getinfo(thread, level, 'f')

    if info == nil then break end

    if info.func == func then
      local i = 1

      while true do
        local name, value = getlocal(thread, level, i)

        if name == nil then break end

        if name == wanted then
          table.insert(found, 1, value)

          break
        end

        i = i + 1
      end
    end

    level = level + 1
  end

  return found
end

local function check_name(name, what, level)
  if type(name) ~= 'string' or name == '' or name:find('%s') or name:sub(1, 1) == ':'
    or name:sub(-1) == ':' or name:sub(1, 1) == '-' or name:find('=', 1, true) then
    error(what..' must be a name without blanks or "=" that neither starts with ":" or "-" nor ends with ":", not '..
      (type(name) == 'string' and "'"..name.."'" or 'a '..type(name)), level + 1)
  end
end

function namespace(name, fn)
  check_name(name, 'a namespace', 2)

  if type(fn) ~= 'function' then
    error('namespace(name, fn): fn must be a function, not a '..type(fn), 2)
  end

  local namespace_name = name

  fn()
end

local namespace_function = namespace

--- The prefix of the namespaces open now: `db:migrate:` inside `namespace('migrate')` in
-- `namespace('db')`, '' outside of any.
-- @return [String]
local function prefix()
  local open = frames_of(running_thread() or main, namespace_function, 'namespace_name')

  if #open == 0 then return '' end

  return table.concat(open, ':')..':'
end

local function absolute(name)
  if type(name) == 'string' and name:sub(1, 1) == ':' then
    return prefix()..name:sub(2)
  end

  return name
end

function desc(text)
  if text ~= nil and type(text) ~= 'string' and type(text) ~= 'number' then
    error('desc(text): text must be a string, not a '..type(text), 2)
  end

  pending_description = text ~= nil and tostring(text) or nil
end

function task(name, prerequisites, fn)
  check_name(name, 'a task', 2)

  if type(prerequisites) == 'function' and fn == nil then
    prerequisites, fn = nil, prerequisites
  end

  if type(prerequisites) == 'string' then
    prerequisites = { prerequisites }
  end

  if prerequisites ~= nil and type(prerequisites) ~= 'table' then
    error('task(name, prerequisites, fn): prerequisites must be a list of task names, not a '..type(prerequisites), 2)
  end

  if fn ~= nil and type(fn) ~= 'function' then
    error('task(name, prerequisites, fn): fn must be a function, not a '..type(fn), 2)
  end

  local full = prefix()..name
  local t = tasks[full]

  if not t then
    t = { name = full, prerequisites = {}, actions = {}, package = current_package }
    tasks[full] = t
    order[#order + 1] = full
  end

  if pending_description then
    t.description = pending_description
    pending_description = nil
  end

  for _, p in ipairs(prerequisites or {}) do
    local resolved = absolute(p)

    check_name(type(resolved) == 'string' and resolved or p, 'a prerequisite', 2)

    local present = false

    for _, q in ipairs(t.prerequisites) do
      if q == resolved then present = true end
    end

    if not present then
      t.prerequisites[#t.prerequisites + 1] = resolved
    end
  end

  if fn then
    t.actions[#t.actions + 1] = fn
  end
end

local function copy_options(options)
  local out = {}

  for k, v in pairs(options or {}) do
    out[k] = v
  end

  return out
end

local function copy_words(words)
  local out = {}

  for i, w in ipairs(words or {}) do
    out[i] = w
  end

  return out
end

local run_task
local started = {}

--- The tasks running now, outermost first, each with how it was reached: `{ name, by,
-- thread }`, `by` being the task that needs it as a prerequisite, or nil when it was
-- invoked. Those that no longer run are forgotten.
-- @return [Table list]
local function running()
  local current = running_thread() or main
  local live, read, kept = {}, {}, {}

  for _, entry in ipairs(started) do
    local thread = entry.thread

    if not read[thread] and (thread == current or status(thread) == 'normal') then
      read[thread] = true

      for _, found in ipairs(frames_of(thread, run_task, 'running_task')) do
        live[found] = true
      end
    end

    if live[entry] then kept[#kept + 1] = entry end
  end

  started = kept

  return kept
end

--- Runs a task: its prerequisites, then its actions, unless it has run already.
-- @param name [String full name of the task]
-- @param options [Table KEY=VALUE words]
-- @param words [Table the other words]
-- @param needed_by=nil [String the task that has it as a prerequisite]
run_task = function(name, options, words, needed_by)
  local t = tasks[name]

  if not t then
    if needed_by then
      error("unknown task '"..tostring(name).."', a prerequisite of '"..needed_by.."'", 0)
    end

    error("unknown task '"..tostring(name).."'", 0)
  end

  if done[name] then return end

  local chain = running()

  for i, r in ipairs(chain) do
    if r.name == name then
      local names = {}
      local invoked = needed_by == nil

      for j = i, #chain do
        names[#names + 1] = chain[j].name

        if j > i and chain[j].by == nil then invoked = true end
      end

      names[#names + 1] = name

      if invoked then
        error("'"..name.."' is still running, and invoking it again makes a cycle: "..table.concat(names, ' -> '), 0)
      end

      error('a cycle of prerequisites: '..table.concat(names, ' -> '), 0)
    end
  end

  local running_task = { name = name, by = needed_by, thread = running_thread() or main }

  started[#started + 1] = running_task

  for _, p in ipairs(t.prerequisites) do
    run_task(p, options, words, name)
  end

  for _, action in ipairs(t.actions) do
    action({ options = copy_options(options), words = copy_words(words), name = name })
  end

  for i = #started, 1, -1 do
    if started[i] == running_task then
      table.remove(started, i)

      break
    end
  end

  done[name] = true
end

--- A name given to `Tasks.invoke` or `Tasks.exists`: one that starts with `:` is relative to
-- the namespace of the task that is running, or to the namespaces open while the task files
-- load.
-- @param name [String]
-- @return [String full name]
local function resolve(name)
  if name:sub(1, 1) ~= ':' then return name end

  local chain = running()
  local current = chain[#chain]

  if current then
    local space = current.name:match('^(.*):[^:]*$')

    return (space and space..':' or '')..name:sub(2)
  end

  return prefix()..name:sub(2)
end

Tasks = {}

function Tasks.invoke(name, args)
  if type(name) ~= 'string' then
    error('Tasks.invoke(name, args): name must be a string, not a '..type(name), 2)
  end

  args = args or {}
  run_task(resolve(name), args.options, args.words)
end

function Tasks.exists(name)
  return type(name) == 'string' and tasks[resolve(name)] ~= nil
end

local host = {}

function host.begin_file(package_id)
  current_package = package_id
  pending_description = nil
end

function host.end_file()
  current_package = nil
  pending_description = nil
end

function host.list()
  local out = {}

  for i, name in ipairs(order) do
    local t = tasks[name]

    out[i] = {
      name = name,
      description = t.description,
      prerequisites = t.prerequisites,
      package = t.package,
      actions = #t.actions
    }
  end

  return out
end

function host.invoke(name, options, words)
  run_task(name, options, words)
end

return host
