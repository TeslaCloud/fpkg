-- fpkg's GLua runtime: the `utf8` library on Lua strings.
-- A character is a byte below 0x80, or a lead byte from 0xC2 to 0xF4 followed by as many
-- continuation bytes as it announces (no check of overlong forms, surrogates or the largest
-- code point). `utf8.char` (up to the first nil) and `charpattern`; `utf8.codes`, which
-- steps by whole characters and raises an error at an invalid one; `utf8.len(s, i, j)` and
-- `utf8.codepoint(s, i, j)` over the characters that start between byte positions `i` and
-- `j` (a position of 0 or less counts from the end, 0 being the end, and is at least 1; past
-- the end plus one is an error), `len` giving false and the position of an invalid sequence,
-- `codepoint` raising an error there and always decoding the character at `i`;
-- `utf8.offset(s, n, i)`, the position of the character `n` characters after (or before) the
-- one at `i`, which must start a character; `utf8.force`, each invalid byte replaced with
-- U+FFFD; and `utf8.sub` and `utf8.GetChar`, `string.sub` and `string.GetChar` counted in
-- characters of a valid string.

local byte, char, sub, format = string.byte, string.char, string.sub, string.format
local floor = math.floor
local concat = table.concat
local tonumber, type, error = tonumber, type, error

utf8 = utf8 or {}

local MAX_CODE = 0x10FFFF

utf8.charpattern = '[%z\1-\127\194-\244][\128-\191]*'

local function is_continuation(b)
  return b ~= nil and b >= 0x80 and b < 0xC0
end

local sequences = {
  { base = 0xC0, low = 0xC2, high = 0xDF, extra = 1 },
  { base = 0xE0, low = 0xE0, high = 0xEF, extra = 2 },
  { base = 0xF0, low = 0xF0, high = 0xF4, extra = 3 }
}

local function decode(text, position)
  local lead = byte(text, position)

  if lead == nil then
    return nil
  elseif lead < 0x80 then
    return lead, position + 1
  end

  for _, shape in ipairs(sequences) do
    if lead >= shape.low and lead <= shape.high then
      local code = lead - shape.base

      for offset = 1, shape.extra do
        local follow = byte(text, position + offset)

        if not is_continuation(follow) then
          return nil
        end

        code = code * 64 + (follow - 0x80)
      end

      return code, position + shape.extra + 1
    end
  end

  return nil
end

local function encode(code)
  if code < 0x80 then
    return char(code)
  elseif code < 0x800 then
    return char(0xC0 + floor(code / 0x40), 0x80 + code % 0x40)
  elseif code < 0x10000 then
    return char(0xE0 + floor(code / 0x1000), 0x80 + floor(code / 0x40) % 0x40, 0x80 + code % 0x40)
  end

  return char(
    0xF0 + floor(code / 0x40000),
    0x80 + floor(code / 0x1000) % 0x40,
    0x80 + floor(code / 0x40) % 0x40,
    0x80 + code % 0x40
  )
end

local function argument_error(n, func_name, message)
  error(format("bad argument #%d to '%s' (%s)", n, func_name, message), 3)
end

local function check_text(text, func_name)
  local kind = type(text)

  if kind == 'number' then
    return tostring(text)
  elseif kind ~= 'string' then
    argument_error(1, func_name, 'string expected, got '..(kind == 'nil' and 'no value' or kind))
  end

  return text
end

local function integer_argument(value, n, func_name, default)
  if value == nil then
    return default
  end

  local number = tonumber(value)

  if number == nil then
    argument_error(n, func_name, 'number expected, got '..type(value))
  end

  return floor(number)
end

local function absolute(position, length)
  if position <= 0 then
    position = length + position + 1
  end

  if position < 1 then
    return 1
  end

  return position
end

local function starts_character(text, position)
  return decode(text, position) ~= nil
end

function utf8.char(...)
  local pieces = {}

  for i, code in ipairs({ ... }) do
    if code < 0 or code > MAX_CODE then
      error('bad argument #'..i..' to char (out of range)', 2)
    end

    pieces[i] = encode(code)
  end

  return concat(pieces)
end

function utf8.codepoint(text, first, last)
  text = check_text(text, 'codepoint')

  local length = #text
  local from = absolute(integer_argument(first, 2, 'codepoint', 1), length)
  local to = absolute(integer_argument(last, 3, 'codepoint', from), length)

  if from > length + 1 or to > length + 1 then
    error('bad index to string (out of range)', 2)
  end

  local codes = {}
  local position = from

  repeat
    local code, after = decode(text, position)

    if code == nil then
      error('invalid UTF-8 code', 2)
    end

    codes[#codes + 1] = code
    position = after
  until position > to

  return unpack(codes)
end

function utf8.len(text, first, last)
  text = check_text(text, 'len')

  local length = #text
  local from = absolute(integer_argument(first, 2, 'len', 1), length)
  local to = absolute(integer_argument(last, 3, 'len', -1), length)

  if from > length + 1 or to > length + 1 then
    error('bad index to string (out of range)', 2)
  end

  local count = 0
  local position = from

  while position <= to and position <= length do
    local code, after = decode(text, position)

    if code == nil then
      return false, position
    end

    count = count + 1
    position = after
  end

  return count
end

function utf8.offset(text, n, start)
  text = check_text(text, 'offset')
  n = integer_argument(n, 2, 'offset', nil)

  if n == nil then
    argument_error(2, 'offset', 'number expected, got no value')
  end

  local length = #text
  local position

  if start == nil then
    position = n >= 0 and 1 or length
  else
    position = integer_argument(start, 3, 'offset', nil)

    if position < 0 then
      position = length + position + 1
    end

    if position < 1 or position > length then
      error('bad index to string (out of range)', 2)
    end
  end

  if n == 0 then
    while position > 0 and not starts_character(text, position) do
      position = position - 1
    end

    return position
  end

  if not starts_character(text, position) then
    error('initial position is a continuation byte', 2)
  end

  if n < 0 then
    while n < 0 do
      repeat
        position = position - 1
      until position < 1 or starts_character(text, position)

      if position < 1 then
        return nil
      end

      n = n + 1
    end

    return position
  end

  while n > 0 and position <= length do
    repeat
      position = position + 1
    until position > length or starts_character(text, position)

    n = n - 1
  end

  if n == 0 and position <= length then
    return position
  end

  return nil
end

local function step(text, previous)
  local position = 1

  if previous >= 1 then
    local _, after = decode(text, previous)

    position = after
  end

  if position > #text then
    return nil
  end

  local code = decode(text, position)

  if code == nil then
    error('invalid UTF-8 code', 2)
  end

  return position, code
end

function utf8.codes(text)
  text = check_text(text, 'codes')

  return step, text, 0
end

local REPLACEMENT = '\239\191\189'

function utf8.force(text)
  text = check_text(text, 'force')

  local pieces = {}
  local position, length = 1, #text

  while position <= length do
    local code, after = decode(text, position)

    if code == nil then
      pieces[#pieces + 1] = REPLACEMENT
      position = position + 1
    else
      pieces[#pieces + 1] = sub(text, position, after - 1)
      position = after
    end
  end

  return concat(pieces)
end

local function byte_span(text, first, last)
  local index, position = 1, 1
  local from

  while index <= last do
    if index == first then
      from = position
    end

    local _, after = decode(text, position)

    position = after
    index = index + 1
  end

  return from, position - 1
end

function utf8.sub(text, first, last)
  text = check_text(text, 'sub')
  last = last or -1

  local count = utf8.len(text)

  if first < 0 then
    first = count + first + 1
  end

  if last < 0 then
    last = count + last + 1
  end

  if first < 1 then
    first = 1
  end

  if first > last then
    return ''
  end

  if last > count then
    last = count
  end

  if first > last then
    return ''
  end

  return sub(text, byte_span(text, first, last))
end

function utf8.GetChar(text, index)
  return utf8.sub(text, index, index)
end
