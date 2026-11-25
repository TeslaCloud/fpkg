--- Tasks of the end-to-end fixture: namespaces, prerequisites, arguments, failures, files,
-- the database and the loop.

desc 'Prepare the environment'
task('environment', function()
  environment_runs = (environment_runs or 0) + 1
  print('environment '..environment_runs)
end)

task('helpers:greet', { 'environment' })

namespace('db', function()
  desc 'Create the table of visits'
  task('setup', { 'environment' }, function()
    assert(sql.Query('CREATE TABLE IF NOT EXISTS visits (id INTEGER PRIMARY KEY AUTOINCREMENT, note TEXT)') == nil)
  end)

  desc 'Add a visit and count them'
  task('count', { 'environment', ':setup' }, function(args)
    sql.QueryTyped('INSERT INTO visits (note) VALUES (?)', args.options.NOTE)
    print('visits '..sql.QueryValue('SELECT count(*) FROM visits'))
  end)
end)

desc 'Print the arguments'
task('args', function(args)
  local keys = {}

  for k, v in SortedPairs(args.options) do
    keys[#keys + 1] = k..'='..v
  end

  print('name '..args.name)
  print('options '..table.concat(keys, ' '))
  print('words '..table.concat(args.words, ' '))
end)

desc 'Fail on purpose'
task('fail', function()
  local function deeper()
    error('it failed on purpose', 0)
  end

  deeper()
end)

desc 'Write files through file and File'
task('files', function()
  require 'file'

  file.CreateDir('notes')
  assert(file.Write('notes/visit.txt', 'data file'))
  assert(File.write('gamemodes/taskgm/generated/out.lua', 'return 1'))
  print('read '..file.Read('notes/visit.txt', 'DATA')..', '..File.read('gamemodes/taskgm/generated/out.lua'))
  print('found '..table.concat(file.Find('taskgm/generated/*.lua', 'LUA'), ','))
end)

desc 'Wait for a timer'
task('wait', function()
  local done = false

  timer.Simple(0.05, function() done = true end)
  print('waited '..tostring(fpkg.wait_until(function() return done end, 10)))
  timer.Simple(0, function() print('delivered after the task') end)
end)

desc 'Exit with code 3'
task('exit', function()
  fpkg.exit(3)
end)

desc 'Print what fpkg tells tasks'
task('host', function()
  print('env '..tostring(fpkg.env))
  print('server '..tostring(fpkg.server))
  print('project '..fpkg.project.id..' '..fpkg.project.path)

  local ids = {}

  for _, package in ipairs(fpkg.packages) do
    ids[#ids + 1] = package.id
  end

  print('packages '..table.concat(ids, ' '))
  print('gamemode '..engine.ActiveGamemode())
end)

task('hidden', function()
  print('hidden ran')
end)

task('late', function()
  timer.Simple(0, function() error('late failure') end)
end)

task('install', function()
  error('a task named like a command failed')
end)
