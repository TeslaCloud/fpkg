-- fpkg's GLua runtime: the parts of the engine's libraries that need no game, and harmless
-- stand-ins for those that would talk to clients.
-- `system.IsLinux`, `IsWindows` and `IsOSX` (the platform fpkg runs on), `AppTime`, `UpTime`,
-- `SteamTime`, `BatteryPower`; `game.IsDedicated` (true), `SinglePlayer` (false) and
-- `GetMap`, which fails loudly: there is no map, and data keyed by map must not be read or
-- written under a made-up name; `gmod.GetGamemode` (`GAMEMODE`, else `GM`); the player list
-- of a server nobody is on (`player.GetAll`, `GetHumans`, `GetBots` are empty, `GetCount` 0,
-- `Iterator` ends at once); `resource.AddFile`, `AddSingleFile` and `AddWorkshop`, which
-- record nothing; `util.AddNetworkString`, `NetworkStringToID` and `NetworkIDToString`;
-- `net.Receive` (kept in `net.Receivers`), `net.Start`, the `net.Write*` functions and the
-- sends, which reach nobody; `util.PrecacheModel`, `util.PrecacheSound`, `Sound`, `Model` and
-- `Particle`. `util.IsValidModel` is left to fail loudly, since without the game's content
-- no answer would be true.

system = system or {}

local os_name = jit and jit.os or ''

function system.IsLinux() return os_name == 'Linux' end
function system.IsWindows() return os_name == 'Windows' end
function system.IsOSX() return os_name == 'OSX' end
function system.AppTime() return SysTime() end
function system.UpTime() return math.floor(SysTime()) end
function system.SteamTime() return os.time() end
function system.BatteryPower() return 255 end

game = game or {}

function game.GetMap()
  error("fpkg's runtime has no map: game.GetMap is not available in tasks", 2)
end
function game.IsDedicated() return true end
function game.SinglePlayer() return false end

gmod = gmod or {}

function gmod.GetGamemode()
  return GAMEMODE or GM
end

player = player or {}

function player.GetAll() return {} end
function player.GetHumans() return {} end
function player.GetBots() return {} end
function player.GetCount() return 0 end
function player.GetCountConnecting() return 0 end

function player.Iterator()
  return function() end, {}, 0
end

resource = resource or {}

function resource.AddFile() end
function resource.AddSingleFile() end
function resource.AddWorkshop() end

do
  local ids, names = {}, {}

  function util.AddNetworkString(name)
    local lower = string.lower(name)

    if not ids[lower] then
      names[#names + 1] = name
      ids[lower] = #names
    end

    return ids[lower]
  end

  function util.NetworkStringToID(name)
    return ids[string.lower(name)] or 0
  end

  function util.NetworkIDToString(id)
    return names[id]
  end
end

net = net or {}
net.Receivers = net.Receivers or {}

function net.Receive(name, func)
  net.Receivers[string.lower(name)] = func
end

for _, name in ipairs({
  'Start', 'Send', 'SendOmit', 'SendPAS', 'SendPVS', 'Broadcast', 'Abort',
  'WriteAngle', 'WriteBit', 'WriteBool', 'WriteColor', 'WriteData', 'WriteDouble', 'WriteEntity',
  'WriteFloat', 'WriteInt', 'WriteMatrix', 'WriteNormal', 'WritePlayer', 'WriteString',
  'WriteTable', 'WriteType', 'WriteUInt', 'WriteUInt64', 'WriteVector'
}) do
  net[name] = function() end
end

function net.BytesWritten() return 0, 0 end

function util.PrecacheModel() end
function util.PrecacheSound() end

function Sound(name)
  util.PrecacheSound(name)

  return name
end

function Model(name)
  util.PrecacheModel(name)

  return name
end

function Particle(name)
  return name
end
