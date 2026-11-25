-- fpkg's GLua runtime: the game's additions to the `string` library, and indexing strings.
-- Splitting and joining (`Explode` with plain or pattern separators, finding at most as many
-- separators as the text has characters, so that a pattern matching the empty string gives
-- empty pieces; `Split`, `Implode`, `ToTable`), file names (`GetExtensionFromFilename`,
-- `StripExtension`, `GetPathFromFilename`, `GetFileFromFilename`), times and sizes
-- (`FormattedTime`, `ToMinutesSeconds`, `ToMinutesSecondsMilliseconds`, `NiceTime`, "a few
-- seconds" for nil, `NiceSize`), pieces (`Left`, `Right`, `GetChar`, `SetChar`,
-- `StartsWith`, `StartWith`, `EndsWith`, `Trim`, `TrimLeft`, `TrimRight`, `Replace`), escaping
-- (`PatternSafe`, `JavascriptSafe`), text (`Comma`, `Interpolate`, which puts in the values of
-- the lookup as `string.gsub` takes them, `CardinalToOrdinal`, `NiceName`) and colors
-- (`ToColor` reads "r g b" or "r g b a" in whole numbers, white otherwise; `FromColor`). A
-- string indexed with a number, or a string holding one, gives its character at that position
-- (`('abc')[2]` is `'b'`); other keys are the functions of `string`.

local lib = string
local find, sub, gsub, match = lib.find, lib.sub, lib.gsub, lib.match
local format, upper = lib.format, lib.upper
local concat = table.concat
local floor = math.floor
local type, tostring, tonumber = type, tostring, tonumber

do
  local meta = getmetatable('')

  meta.__index = function(text, key)
    local value = lib[key]

    if value ~= nil then
      return value
    end

    local index = tonumber(key)

    if index then
      return sub(text, index, index)
    end
  end
end

function lib.ToTable(text)
  text = tostring(text)

  local characters = {}

  for i = 1, #text do
    characters[i] = sub(text, i, i)
  end

  return characters
end

function lib.Explode(separator, text, with_pattern)
  if separator == '' then
    return lib.ToTable(text)
  end

  local plain = not with_pattern
  local pieces = {}
  local position = 1

  while #pieces < #text do
    local first, last = find(text, separator, position, plain)

    if first == nil then
      break
    end

    pieces[#pieces + 1] = sub(text, position, first - 1)
    position = last + 1
  end

  pieces[#pieces + 1] = sub(text, position)

  return pieces
end

function lib.Split(text, separator)
  return lib.Explode(separator, text)
end

function lib.Implode(separator, pieces)
  return concat(pieces, separator)
end

function lib.GetExtensionFromFilename(path)
  return match(path, '%.([^%./\\]*)$')
end

function lib.StripExtension(path)
  local stripped = match(path, '^(.*)%.[^%./\\]*$')

  return stripped or path
end

function lib.GetPathFromFilename(path)
  return match(path, '^(.*[/\\])') or ''
end

function lib.GetFileFromFilename(path)
  return match(path, '([^/\\]*)$')
end

local function clock_parts(seconds)
  local hours = floor(seconds / 3600)
  local minutes = floor(seconds / 60) % 60
  local whole = floor(seconds) % 60
  local thousandths = (seconds - floor(seconds)) * 1000

  return hours, minutes, whole, thousandths
end

function lib.FormattedTime(seconds, layout)
  local hours, minutes, whole, thousandths = clock_parts(seconds)

  if layout then
    return format(layout, minutes, whole, thousandths)
  end

  return { h = hours, m = minutes, s = whole, ms = thousandths }
end

function lib.ToMinutesSeconds(seconds)
  local _, minutes, whole = clock_parts(seconds)

  return format('%02i:%02i', minutes, whole)
end

function lib.ToMinutesSecondsMilliseconds(seconds)
  local _, minutes, whole, thousandths = clock_parts(seconds)

  return format('%02i:%02i:%02i', minutes, whole, thousandths)
end

local time_units = {
  { 1, 'second', 60 },
  { 60, 'minute', 3600 },
  { 3600, 'hour', 86400 },
  { 86400, 'day', 604800 },
  { 604800, 'week', 31536000 },
  { 31536000, 'year', math.huge }
}

function lib.NiceTime(seconds)
  if seconds == nil then
    return 'a few seconds'
  end

  for _, unit in ipairs(time_units) do
    if seconds < unit[3] then
      local count = floor(seconds / unit[1])

      return count..' '..unit[2]..(count == 1 and '' or 's')
    end
  end
end

local size_units = {
  { 1e9, 'GB' },
  { 1e6, 'MB' },
  { 1e3, 'KB' }
}

function lib.NiceSize(bytes)
  bytes = tonumber(bytes) or 0

  if bytes <= 0 then
    return '0'
  end

  for _, unit in ipairs(size_units) do
    if bytes >= unit[1] then
      return math.Round(bytes / unit[1], 2)..' '..unit[2]
    end
  end

  return bytes..' Bytes'
end

function lib.Left(text, count)
  return sub(text, 1, count)
end

function lib.Right(text, count)
  return sub(text, -count)
end

function lib.GetChar(text, index)
  return sub(text, index, index)
end

function lib.SetChar(text, index, character)
  return sub(text, 1, index - 1)..character..sub(text, index + 1)
end

function lib.StartsWith(text, start)
  return sub(text, 1, #start) == start
end

lib.StartWith = lib.StartsWith

function lib.EndsWith(text, ending)
  return ending == '' or sub(text, -#ending) == ending
end

local magic = '[%^%$%(%)%%%.%[%]%*%+%-%?]'

function lib.PatternSafe(text)
  return (gsub(gsub(text, magic, '%%%0'), '%z', '%%z'))
end

local function repeated(characters)
  if characters == nil then
    return '%s*'
  end

  return lib.PatternSafe(characters)..'*'
end

function lib.Trim(text, characters)
  local run = repeated(characters)

  return match(text, '^'..run..'(.-)'..run..'$') or text
end

function lib.TrimLeft(text, characters)
  return (gsub(text, '^'..repeated(characters), ''))
end

function lib.TrimRight(text, characters)
  return (gsub(text, repeated(characters)..'$', ''))
end

function lib.Replace(text, search, replacement)
  if search == '' then
    return concat(lib.ToTable(text), replacement)
  end

  local pieces = {}
  local position = 1

  while true do
    local first, last = find(text, search, position, true)

    if first == nil then
      break
    end

    pieces[#pieces + 1] = sub(text, position, first - 1)
    pieces[#pieces + 1] = replacement
    position = last + 1
  end

  pieces[#pieces + 1] = sub(text, position)

  return concat(pieces)
end

local javascript_escapes = {
  ['\0'] = '\\x00',
  ['\b'] = '\\b',
  ['\t'] = '\\t',
  ['\n'] = '\\n',
  ['\v'] = '\\v',
  ['\f'] = '\\f',
  ['\r'] = '\\r'
}

local function javascript_escape(character)
  return javascript_escapes[character] or '\\'..character
end

function lib.JavascriptSafe(text)
  text = gsub(text, '[%z\b\t\n\v\f\r"$\'\\`{}]', javascript_escape)

  return (gsub(text, '\226\128[\168\169]', '\\%0'))
end

function lib.Comma(value, separator)
  separator = separator or ','

  if find(separator, '%d') then
    error("bad argument #2 to 'string.Comma' (non-numerical values expected, got "..separator..')', 2)
  end

  local text

  if type(value) == 'number' then
    text = gsub(format('%f', value), '%.?0+$', '')
  else
    text = tostring(value)
  end

  local sign, digits, rest = match(text, '^([%-%+]?)(%d+)(.*)$')

  if digits == nil then
    return text
  end

  local groups = {}
  local head = #digits % 3

  if head > 0 then
    groups[1] = sub(digits, 1, head)
  end

  for i = head + 1, #digits, 3 do
    groups[#groups + 1] = sub(digits, i, i + 2)
  end

  return sign..concat(groups, separator)..rest
end

function lib.Interpolate(text, values)
  return (gsub(text, '{([%w_]+)}', function(key)
    return values[key]
  end))
end

function lib.CardinalToOrdinal(number)
  return tostring(number)..STNDRD(number)
end

function lib.NiceName(text)
  text = gsub(text, '_', ' ')
  text = gsub(text, '(%l)([^%l ])', '%1 %2')

  return upper(sub(text, 1, 1))..sub(text, 2)
end

function lib.ToColor(text)
  local r, g, b, a = match(text, '(%d+) (%d+) (%d+) (%d+)')

  if r == nil then
    r, g, b = match(text, '(%d+) (%d+) (%d+)')
  end

  if r == nil then
    return Color(255, 255, 255, 255)
  end

  return Color(tonumber(r), tonumber(g), tonumber(b), tonumber(a) or 255)
end

function lib.FromColor(color)
  return format('%i %i %i %i', color.r, color.g, color.b, color.a)
end
