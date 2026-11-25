-- fpkg's GLua runtime: the game's additions to the `math` library.
-- Numbers (`Clamp`, `Round`, `Truncate`, `SnapTo`, `Sign`, `Approach`, `Remap`,
-- `TimeFraction`, `IsNearlyEqual`, `Rand`, `Factorial`, `Max`, `Min`; `IntToBin`, whose
-- digits come in groups of three, a negative number in 64-bit two's complement, and
-- `BinToInt`), angles in degrees (`NormalizeAngle`, `AngleDifference`, `ApproachAngle`),
-- distances (`Distance`, `Dist`, `DistanceSqr`), curves (`QuadraticBezier`, `CubicBezier` and
-- `CHSpline`, its fraction kept within 0-1, over numbers or vectors; `BSplinePoint`, a uniform
-- cubic B-spline of vectors, on `calcBSplineN`, whose terms with an empty span are 0),
-- `EaseInOut` (ease fractions that add up to more than one are scaled down), the easing
-- functions of `math.ease` (those of easings.net) and `math.tau`.

local lib = math
local floor, ceil, abs, sqrt, sin, cos, pi = lib.floor, lib.ceil, lib.abs, lib.sqrt, lib.sin, lib.cos, lib.pi
local random, min, max = lib.random, lib.min, lib.max
local tonumber = tonumber
local concat = table.concat

lib.tau = pi * 2

function lib.Clamp(value, low, high)
  return min(max(value, low), high)
end

function lib.Round(value, decimals)
  local scale = 10 ^ (decimals or 0)

  return floor(value * scale + 0.5) / scale
end

function lib.Truncate(value, decimals)
  local scale = 10 ^ (decimals or 0)
  local scaled = value * scale

  if scaled < 0 then
    return ceil(scaled) / scale
  end

  return floor(scaled) / scale
end

function lib.SnapTo(value, step)
  return floor(value / step + 0.5) * step
end

function lib.Sign(value)
  if value > 0 then
    return 1
  elseif value < 0 then
    return -1
  end

  return 0
end

function lib.Approach(current, target, change)
  change = abs(change)

  if current < target then
    local moved = current + change

    if moved > target then
      return target
    end

    return moved
  elseif current > target then
    local moved = current - change

    if moved < target then
      return target
    end

    return moved
  end

  return target
end

function lib.Remap(value, in_low, in_high, out_low, out_high)
  return out_low + (value - in_low) / (in_high - in_low) * (out_high - out_low)
end

function lib.TimeFraction(start, finish, current)
  return (current - start) / (finish - start)
end

function lib.IsNearlyEqual(a, b, tolerance)
  return abs(a - b) <= (tolerance or 1e-8)
end

function lib.Rand(low, high)
  return low + (high - low) * random()
end

function lib.Factorial(n)
  if n < 0 then
    return nil
  end

  local product = 1

  for i = 2, n do
    product = product * i
  end

  return product
end

local function binary_digits(value, width)
  local digits = {}

  repeat
    digits[#digits + 1] = value % 2
    value = floor(value / 2)
  until value <= 0 and #digits >= width

  return digits
end

function lib.IntToBin(value)
  value = tonumber(value) or 0

  if value < 0 then
    value = ceil(value)
  else
    value = floor(value)
  end

  if value == 0 then
    value = 0
  end

  local digits

  if value < 0 then
    digits = binary_digits(-value - 1, 64)

    for i = 1, #digits do
      digits[i] = 1 - digits[i]
    end
  else
    digits = binary_digits(value, 1)
  end

  while #digits % 3 ~= 0 do
    digits[#digits + 1] = 0
  end

  local count = #digits

  for i = 1, floor(count / 2) do
    digits[i], digits[count - i + 1] = digits[count - i + 1], digits[i]
  end

  return concat(digits)
end

function lib.BinToInt(text)
  return tonumber(text, 2)
end

function lib.NormalizeAngle(angle)
  return (angle + 180) % 360 - 180
end

function lib.AngleDifference(a, b)
  return lib.NormalizeAngle(a - b)
end

function lib.ApproachAngle(current, target, rate)
  local difference = lib.AngleDifference(target, current)

  return lib.Approach(current, current + difference, rate)
end

function lib.DistanceSqr(x1, y1, x2, y2)
  local dx, dy = x2 - x1, y2 - y1

  return dx * dx + dy * dy
end

function lib.Distance(x1, y1, x2, y2)
  return sqrt(lib.DistanceSqr(x1, y1, x2, y2))
end

lib.Dist = lib.Distance
lib.Max = lib.max
lib.Min = lib.min

function lib.QuadraticBezier(fraction, p0, p1, p2)
  local rest = 1 - fraction

  return rest * rest * p0 + 2 * rest * fraction * p1 + fraction * fraction * p2
end

function lib.CubicBezier(fraction, p0, p1, p2, p3)
  local rest = 1 - fraction
  local rest2, fraction2 = rest * rest, fraction * fraction

  return rest2 * rest * p0 + 3 * rest2 * fraction * p1 + 3 * rest * fraction2 * p2 + fraction2 * fraction * p3
end

function lib.CHSpline(fraction, p0, tangent0, p1, tangent1)
  fraction = lib.Clamp(fraction, 0, 1)

  local f2 = fraction * fraction
  local f3 = f2 * fraction

  return (2 * f3 - 3 * f2 + 1) * p0 + (f3 - 2 * f2 + fraction) * tangent0 + (-2 * f3 + 3 * f2) * p1 + (f3 - f2) * tangent1
end

local function knot(i, step)
  return (i - 3) * step
end

function lib.calcBSplineN(i, k, t, step)
  if k == 1 then
    if knot(i, step) <= t and t < knot(i + 1, step) then
      return 1
    end

    return 0
  end

  local start, after, last, finish = knot(i, step), knot(i + 1, step), knot(i + k - 1, step), knot(i + k, step)
  local rising, falling = 0, 0

  if last ~= start then
    rising = lib.calcBSplineN(i, k - 1, t, step) * (t - start) / (last - start)
  end

  if finish ~= after then
    falling = lib.calcBSplineN(i + 1, k - 1, t, step) * (finish - t) / (finish - after)
  end

  return rising + falling
end

function lib.BSplinePoint(fraction, points, fraction_max)
  local step = fraction_max / (#points - 3)
  local at = fraction + step
  local point = Vector(0, 0, 0)

  for i = 1, #points do
    point = point + points[i] * lib.calcBSplineN(i, 4, at, step)
  end

  return point
end

function lib.EaseInOut(progress, ease_in, ease_out)
  ease_in = ease_in or 0
  ease_out = ease_out or 1

  local total = ease_in + ease_out

  if total > 1 then
    ease_in, ease_out = ease_in / total, ease_out / total
  end

  local speed = 2 / (2 - ease_in - ease_out)

  if progress < ease_in then
    return speed / (2 * ease_in) * progress * progress
  elseif progress <= 1 - ease_out then
    return speed * (progress - ease_in / 2)
  end

  local rest = 1 - progress

  return 1 - speed / (2 * ease_out) * rest * rest
end

local ease = {}

lib.ease = ease

local BACK = 1.70158
local BACK_IN_OUT = BACK * 1.525
local BACK_OUT = BACK + 1
local ELASTIC = (2 * pi) / 3
local ELASTIC_IN_OUT = (2 * pi) / 4.5
local BOUNCE = 7.5625
local BOUNCE_SPAN = 2.75

function ease.InSine(x) return 1 - cos((x * pi) / 2) end
function ease.OutSine(x) return sin((x * pi) / 2) end
function ease.InOutSine(x) return -(cos(pi * x) - 1) / 2 end

function ease.InQuad(x) return x * x end
function ease.OutQuad(x) return 1 - (1 - x) * (1 - x) end

function ease.InOutQuad(x)
  if x < 0.5 then
    return 2 * x * x
  end

  return 1 - (-2 * x + 2) ^ 2 / 2
end

function ease.InCubic(x) return x ^ 3 end
function ease.OutCubic(x) return 1 - (1 - x) ^ 3 end

function ease.InOutCubic(x)
  if x < 0.5 then
    return 4 * x ^ 3
  end

  return 1 - (-2 * x + 2) ^ 3 / 2
end

function ease.InQuart(x) return x ^ 4 end
function ease.OutQuart(x) return 1 - (1 - x) ^ 4 end

function ease.InOutQuart(x)
  if x < 0.5 then
    return 8 * x ^ 4
  end

  return 1 - (-2 * x + 2) ^ 4 / 2
end

function ease.InQuint(x) return x ^ 5 end
function ease.OutQuint(x) return 1 - (1 - x) ^ 5 end

function ease.InOutQuint(x)
  if x < 0.5 then
    return 16 * x ^ 5
  end

  return 1 - (-2 * x + 2) ^ 5 / 2
end

function ease.InExpo(x)
  if x == 0 then
    return 0
  end

  return 2 ^ (10 * x - 10)
end

function ease.OutExpo(x)
  if x == 1 then
    return 1
  end

  return 1 - 2 ^ (-10 * x)
end

function ease.InOutExpo(x)
  if x == 0 or x == 1 then
    return x
  elseif x < 0.5 then
    return 2 ^ (20 * x - 10) / 2
  end

  return (2 - 2 ^ (-20 * x + 10)) / 2
end

function ease.InCirc(x) return 1 - sqrt(1 - x ^ 2) end
function ease.OutCirc(x) return sqrt(1 - (x - 1) ^ 2) end

function ease.InOutCirc(x)
  if x < 0.5 then
    return (1 - sqrt(1 - (2 * x) ^ 2)) / 2
  end

  return (sqrt(1 - (-2 * x + 2) ^ 2) + 1) / 2
end

function ease.InBack(x) return BACK_OUT * x ^ 3 - BACK * x ^ 2 end
function ease.OutBack(x) return 1 + BACK_OUT * (x - 1) ^ 3 + BACK * (x - 1) ^ 2 end

function ease.InOutBack(x)
  if x < 0.5 then
    return ((2 * x) ^ 2 * ((BACK_IN_OUT + 1) * 2 * x - BACK_IN_OUT)) / 2
  end

  return ((2 * x - 2) ^ 2 * ((BACK_IN_OUT + 1) * (x * 2 - 2) + BACK_IN_OUT) + 2) / 2
end

function ease.InElastic(x)
  if x == 0 or x == 1 then
    return x
  end

  return -(2 ^ (10 * x - 10)) * sin((x * 10 - 10.75) * ELASTIC)
end

function ease.OutElastic(x)
  if x == 0 or x == 1 then
    return x
  end

  return 2 ^ (-10 * x) * sin((x * 10 - 0.75) * ELASTIC) + 1
end

function ease.InOutElastic(x)
  if x == 0 or x == 1 then
    return x
  elseif x < 0.5 then
    return -(2 ^ (20 * x - 10) * sin((20 * x - 11.125) * ELASTIC_IN_OUT)) / 2
  end

  return (2 ^ (-20 * x + 10) * sin((20 * x - 11.125) * ELASTIC_IN_OUT)) / 2 + 1
end

function ease.OutBounce(x)
  if x < 1 / BOUNCE_SPAN then
    return BOUNCE * x ^ 2
  elseif x < 2 / BOUNCE_SPAN then
    x = x - 1.5 / BOUNCE_SPAN

    return BOUNCE * x ^ 2 + 0.75
  elseif x < 2.5 / BOUNCE_SPAN then
    x = x - 2.25 / BOUNCE_SPAN

    return BOUNCE * x ^ 2 + 0.9375
  end

  x = x - 2.625 / BOUNCE_SPAN

  return BOUNCE * x ^ 2 + 0.984375
end

function ease.InBounce(x) return 1 - ease.OutBounce(1 - x) end

function ease.InOutBounce(x)
  if x < 0.5 then
    return (1 - ease.OutBounce(1 - 2 * x)) / 2
  end

  return (1 + ease.OutBounce(2 * x - 1)) / 2
end
