-- fpkg's GLua runtime: colors, Lua tables with the `Color` metatable.
-- `Color` (channels of at most 255, alpha 255 by default), `IsColor`, `ColorAlpha`,
-- `ColorRand`; the conversions from and to the HSV, HSL and HWB color spaces (`ColorToHSV`,
-- `HSVToColor`, `ColorToHSL`, `HSLToColor`, `HWBToColor`, whose colors have whole channels
-- within 0-255) and from hexadecimal (`HexToColor`, "#rgb", "#rgba", "#rrggbb" or
-- "#rrggbbaa", the "#" optional); the methods (`Copy`, `Lerp`, `Unpack`, `SetUnpacked`,
-- `ToTable`, `ToVector`, `ToHex`, `ToHSV`, `ToHSL`, `ToHWB` and the `Get`, `Set` and `Add`
-- methods of hue, saturation, brightness, lightness, whiteness and blackness, which keep the
-- component they change within 0-1); `tostring` as "r g b a", equality by channels; and
-- `color_white`, `color_black` and `color_transparent`.

local abs, floor, min, max = math.abs, math.floor, math.min, math.max
local format = string.format
local set_meta, get_meta = setmetatable, getmetatable

local COLOR = {}

COLOR.__index = COLOR

RegisterMetaTable('Color', COLOR)

local function channel(value)
  return min(tonumber(value), 255)
end

function Color(r, g, b, a)
  if a == nil then
    a = 255
  end

  return set_meta({ r = channel(r), g = channel(g), b = channel(b), a = channel(a) }, COLOR)
end

function IsColor(value)
  return get_meta(value) == COLOR
end

function ColorAlpha(color, alpha)
  return Color(color.r, color.g, color.b, alpha)
end

function ColorRand(random_alpha)
  local random = math.random

  return Color(random(0, 255), random(0, 255), random(0, 255), random_alpha and random(0, 255) or 255)
end

COLOR.__tostring = function(self)
  return format('%d %d %d %d', self.r, self.g, self.b, self.a)
end

COLOR.__eq = function(a, b)
  return a.r == b.r and a.g == b.g and a.b == b.b and a.a == b.a
end

local function hue_of(r, g, b, high, spread)
  if spread == 0 then
    return 0
  end

  local hue

  if high == r then
    hue = ((g - b) / spread) % 6
  elseif high == g then
    hue = (b - r) / spread + 2
  else
    hue = (r - g) / spread + 4
  end

  return hue * 60
end

local function unit_channels(color)
  return color.r / 255, color.g / 255, color.b / 255
end

function ColorToHSV(color)
  local r, g, b = unit_channels(color)
  local high, low = max(r, g, b), min(r, g, b)
  local spread = high - low
  local saturation = 0

  if high > 0 then
    saturation = spread / high
  end

  return hue_of(r, g, b, high, spread), saturation, high
end

function ColorToHSL(color)
  local r, g, b = unit_channels(color)
  local high, low = max(r, g, b), min(r, g, b)
  local spread = high - low
  local lightness = (high + low) / 2
  local saturation = 0

  if spread > 0 then
    saturation = spread / (1 - abs(2 * lightness - 1))
  end

  return hue_of(r, g, b, high, spread), saturation, lightness
end

local function color_to_hwb(color)
  local hue, saturation, value = ColorToHSV(color)

  return hue, (1 - saturation) * value, 1 - value
end

local function byte_of(fraction)
  return min(max(floor(fraction * 255), 0), 255)
end

local function unit(value)
  return min(max(value, 0), 1)
end

local function from_chroma(hue, chroma, offset)
  local sector = (hue % 360) / 60
  local second = chroma * (1 - abs(sector % 2 - 1))
  local r, g, b

  if sector < 1 then
    r, g, b = chroma, second, 0
  elseif sector < 2 then
    r, g, b = second, chroma, 0
  elseif sector < 3 then
    r, g, b = 0, chroma, second
  elseif sector < 4 then
    r, g, b = 0, second, chroma
  elseif sector < 5 then
    r, g, b = second, 0, chroma
  else
    r, g, b = chroma, 0, second
  end

  return Color(byte_of(r + offset), byte_of(g + offset), byte_of(b + offset))
end

function HSVToColor(hue, saturation, value)
  local chroma = value * saturation

  return from_chroma(hue, chroma, value - chroma)
end

function HSLToColor(hue, saturation, lightness)
  local chroma = (1 - abs(2 * lightness - 1)) * saturation

  return from_chroma(hue, chroma, lightness - chroma / 2)
end

function HWBToColor(hue, whiteness, blackness)
  local value = unit(1 - blackness)

  return HSVToColor(hue, 1 - whiteness / value, value)
end

local hex_layouts = {
  [3] = { 1, 1, 1 },
  [4] = { 1, 1, 1, 1 },
  [6] = { 2, 2, 2 },
  [8] = { 2, 2, 2, 2 }
}

function HexToColor(hex)
  local digits = hex

  if string.byte(hex, 1) == 35 then
    digits = string.sub(hex, 2)
  end

  local layout = hex_layouts[#digits]

  if not layout or string.find(digits, '[^%x]') then
    error('invalid hex input: '..hex, 2)
  end

  local values = {}
  local position = 1

  for i, width in ipairs(layout) do
    local piece = digits:sub(position, position + width - 1)

    if width == 1 then
      piece = piece..piece
    end

    values[i] = tonumber(piece, 16)
    position = position + width
  end

  return Color(values[1], values[2], values[3], values[4])
end

function COLOR:Copy()
  return Color(self.r, self.g, self.b, self.a)
end

function COLOR:Unpack()
  return self.r, self.g, self.b, self.a
end

function COLOR:SetUnpacked(r, g, b, a)
  self.r = r or 255
  self.g = g or 255
  self.b = b or 255
  self.a = a or 255
end

function COLOR:ToTable()
  return { self.r, self.g, self.b, self.a }
end

function COLOR:ToVector()
  return Vector(self.r / 255, self.g / 255, self.b / 255)
end

function COLOR:Lerp(target, fraction)
  return Color(
    Lerp(fraction, self.r, target.r),
    Lerp(fraction, self.g, target.g),
    Lerp(fraction, self.b, target.b),
    Lerp(fraction, self.a, target.a)
  )
end

function COLOR:ToHex(omit_alpha)
  local text = format('#%02x%02x%02x', self.r, self.g, self.b)

  if not omit_alpha and self.a ~= 255 then
    text = text..format('%02x', self.a)
  end

  return text
end

function COLOR:ToHSV()
  return ColorToHSV(self)
end

function COLOR:ToHSL()
  return ColorToHSL(self)
end

function COLOR:ToHWB()
  return color_to_hwb(self)
end

local function take_channels(self, other)
  self.r, self.g, self.b = other.r, other.g, other.b
end

local spaces = {
  { read = ColorToHSV, write = HSVToColor, names = { 'Hue', 'Saturation', 'Brightness' } },
  { read = ColorToHSL, write = HSLToColor, names = { false, false, 'Lightness' } },
  { read = color_to_hwb, write = HWBToColor, names = { false, 'Whiteness', 'Blackness' } }
}

local function unchanged(value)
  return value
end

for _, space in ipairs(spaces) do
  local read, write = space.read, space.write

  for index, name in ipairs(space.names) do
    if name then
      local limit = index == 1 and unchanged or unit

      COLOR['Get'..name] = function(self)
        return (select(index, read(self)))
      end

      COLOR['Set'..name] = function(self, value)
        local parts = { read(self) }

        parts[index] = limit(value)
        take_channels(self, write(parts[1], parts[2], parts[3]))
      end

      COLOR['Add'..name] = function(self, amount)
        local parts = { read(self) }

        parts[index] = limit(parts[index] + amount)
        take_channels(self, write(parts[1], parts[2], parts[3]))
      end
    end
  end
end

color_white = Color(255, 255, 255)
color_black = Color(0, 0, 0)
color_transparent = Color(255, 255, 255, 0)
