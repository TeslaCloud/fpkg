--- Tasks of the helpers package: one that includes a file of the package through
-- TASKSPEC.path.

local spec = TASKSPEC

namespace('helpers', function()
  desc 'Greet from the helpers package'
  task('greet', function()
    TASKSPEC_ID = spec.id
    print(include(spec.path..'lib/greeting.lua'))
  end)
end)
