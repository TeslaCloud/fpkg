-- The driver of gmod_compare.rs: calls the stock libraries on inputs from a fixed
-- pseudo-random sequence, on fixed cases and in scripted sequences (hooks, console commands,
-- convar callbacks, the class registries), and returns one line per call with the arguments
-- and what came back (or the error), and a count of the calls by library. Results that
-- legitimately depend on the order of `pairs` are compared sorted, error messages without the
-- library's file and line and the names of locals, clocks are fixed, and printed output is
-- captured.

local seed = 12345

local function rand(n)
  seed = (seed * 1103515245 + 12345) % 2147483648

  return seed % n + 1
end

local pieces = {
  'a', 'b', 'Z', ' ', '  ', '\t', '\n', '.', '/', '\\', '%', '(', ')', '_', '-', '1', '42', ',', ':',
  'x.lua', 'dir/', 'é', '\226\128\168', '\0', '"', "'", '{name}', '[', ']', '*', '+', '?', '^', '$',
  'Hello', 'world', 'camelCase', 'SNAKE_case', '\255', '\192', '\224\128', '\240\159\152\128'
}

local function str(max)
  local out = {}

  for i = 1, rand(max or 6) - 1 do
    out[i] = pieces[rand(#pieces)]
  end

  return table.concat(out)
end

local numbers = {
  0, 1, -1, 2, 3, 7, 10, 59, 60, 61, 3599, 3600, 86399, 86400, 604800, 31536000, 1e9, 1e12,
  0.5, 1.5, -1.5, 2.5, 0.1, 0.25, 1 / 3, -0.001, 123.456, 999.999, 1000, 999999, 1e6, 1e15,
  -123456789, 12345.6789, 0.999, 100, 255, 256, -0.5
}

local function num()
  return numbers[rand(#numbers)]
end

local function_names = setmetatable({}, { __mode = 'k' })

local function named(name, f)
  function_names[f] = name

  return f
end

local function dump(v, seen)
  local t = type(v)

  if t == 'function' then return function_names[v] or 'function' end

  if t == 'string' then
    return (string.format('%q', v):gsub('[%z\1-\31\128-\255]', function(c) return '\\'..c:byte() end))
  end
  if t == 'number' then return string.format('%.17g', v) end
  if t ~= 'table' then return tostring(v) end

  seen = seen or {}

  if seen[v] then return '<cycle>' end

  seen[v] = true

  local keys = {}

  for k in pairs(v) do
    keys[#keys + 1] = k
  end

  table.sort(keys, function(a, b)
    if type(a) ~= type(b) then return type(a) < type(b) end
    if type(a) == 'number' or type(a) == 'string' then return a < b end

    return tostring(a) < tostring(b)
  end)

  local out = {}

  for _, k in ipairs(keys) do
    out[#out + 1] = dump(k, seen)..'='..dump(v[k], seen)
  end

  seen[v] = nil

  return '{'..table.concat(out, ',')..'}'
end

local lines = {}
local unordered_comparison = { ['SortedPairs mixed'] = true }

local function normalized(message)
  message = tostring(message):gsub('^fpkg/[%w_]+%.lua:%d+: ', '<library>: ')
  message = message:gsub('^lua/includes/[%w_/]+%.lua:%d+: ', '<library>: ')

  return (message:gsub("(attempt to %a[%a ]-) %a+ '[^']*' %(a (%a+) value%)", '%1 a %2 value'))
end

local function record(name, args, ...)
  local shown = {}

  for i = 1, args.n do
    shown[i] = dump(args[i])
  end

  local results = { pcall(...) }
  local out = {}

  if results[1] then
    for i = 2, #results do
      out[#out + 1] = dump(results[i])
    end

    lines[#lines + 1] = name..'('..table.concat(shown, ', ')..') -> '..table.concat(out, ', ')
  else
    local message = normalized(results[2])

    if unordered_comparison[name] then
      message = message:gsub('attempt to compare (%a+) with (%a+)', function(a, b)
        if b < a then a, b = b, a end

        return 'attempt to compare '..a..' with '..b
      end)
    end
    lines[#lines + 1] = name..'('..table.concat(shown, ', ')..') !! '..dump(message)
  end
end

local function call(name, f, ...)
  local args = { n = select('#', ...), ... }

  record(name, args, f, ...)
end

local function call_mut(name, f, make)
  local args = { make() }
  local target = args[1]

  record(name, { n = 1, args[1] }, function()
    local r = { f(unpack(args)) }

    return dump(target), unpack(r)
  end)
end

for _ = 1, 300 do
  local s, t = str(), str(4)

  call('ToTable', string.ToTable, s)
  call('JavascriptSafe', string.JavascriptSafe, s)
  call('PatternSafe', string.PatternSafe, s)
  call('Explode', string.Explode, t, s)
  call('Explode pattern', string.Explode, ({ '%s', '[,.]', '%d+', '' })[rand(4)], s, true)
  call('Split', string.Split, s, t)
  call('Implode', string.Implode, t, { s, t, 'x' })
  call('GetExtensionFromFilename', string.GetExtensionFromFilename, s)
  call('StripExtension', string.StripExtension, s)
  call('GetPathFromFilename', string.GetPathFromFilename, s)
  call('GetFileFromFilename', string.GetFileFromFilename, s)
  call('FormattedTime', string.FormattedTime, math.abs(num()))
  call('FormattedTime fmt', string.FormattedTime, math.abs(num()), '%02i:%02i:%02i')
  call('ToMinutesSeconds', string.ToMinutesSeconds, math.abs(num()))
  call('ToMinutesSecondsMilliseconds', string.ToMinutesSecondsMilliseconds, math.abs(num()))
  call('NiceTime', string.NiceTime, num())
  call('Left', string.Left, s, rand(8) - 3)
  call('Right', string.Right, s, rand(8) - 3)
  call('Replace', string.Replace, s, t, str(3))
  call('Trim', string.Trim, s)
  call('Trim char', string.Trim, s, pieces[rand(#pieces)])
  call('TrimLeft', string.TrimLeft, s, rand(2) == 1 and pieces[rand(#pieces)] or nil)
  call('TrimRight', string.TrimRight, s, rand(2) == 1 and pieces[rand(#pieces)] or nil)
  call('NiceSize', string.NiceSize, num())
  call('SetChar', string.SetChar, s, rand(6), 'Q')
  call('GetChar', string.GetChar, s, rand(6))
  call('StartsWith', string.StartsWith, s, t)
  call('StartWith', string.StartWith, s, s:sub(1, rand(3)))
  call('EndsWith', string.EndsWith, s, s:sub(-rand(3)))
  call('Comma', string.Comma, num())
  call('Comma str', string.Comma, tostring(num()), rand(2) == 1 and '.' or nil)
  call('Comma bad', string.Comma, num(), '9')
  call('Interpolate', string.Interpolate, s..'{name}{x}', { name = 'N', x = 1 })
  call('CardinalToOrdinal', string.CardinalToOrdinal, rand(130) - 1)
  call('NiceName', string.NiceName, s)
  call('index', function(x, i) return x[i] end, s, rand(5))
  call('ToColor', function(x) return tostring(string.ToColor(x)) end, rand(255)..' '..rand(255)..' '..rand(255))
  call('FromColor', string.FromColor, Color(rand(255), rand(255), rand(255), rand(255)))
end

for _ = 1, 200 do
  local a = num()
  local b, c = num(), num()
  local x = (rand(101) - 1) / 100

  call('Clamp', math.Clamp, a, math.min(b, c), math.max(b, c))
  call('Round', math.Round, a, rand(4) - 1)
  call('Truncate', math.Truncate, a, rand(4) - 1)
  call('Approach', math.Approach, a, b, c)
  call('NormalizeAngle', math.NormalizeAngle, a)
  call('AngleDifference', math.AngleDifference, a, b)
  call('ApproachAngle', math.ApproachAngle, a, b, c)
  call('TimeFraction', math.TimeFraction, a, b, c)
  call('Remap', math.Remap, a, 0, 10, b, c)
  call('SnapTo', math.SnapTo, a, rand(10))
  call('Sign', math.Sign, a)
  call('Distance', math.Distance, a, b, c, x)
  call('DistanceSqr', math.DistanceSqr, a, b, c, x)
  call('Dist', math.Dist, a, b, c, x)
  call('EaseInOut', math.EaseInOut, x, (rand(11) - 1) / 10, (rand(11) - 1) / 10)
  call('EaseInOut nil', math.EaseInOut, x)
  call('CubicBezier', math.CubicBezier, x, a, b, c, 1)
  call('QuadraticBezier', math.QuadraticBezier, x, a, b, c)
  call('Factorial', math.Factorial, rand(15) - 3)
  call('IsNearlyEqual', math.IsNearlyEqual, a, a + x / 1e7)
  call('IntToBin', math.IntToBin, rand(5000) - 1)
  call('BinToInt', math.BinToInt, math.IntToBin(rand(5000) - 1))
  call('calcBSplineN', math.calcBSplineN, rand(6), rand(4), x, 0.25)
  call('Max', math.Max, a, b)
  call('Min', math.Min, a, b)

  for _, name in ipairs({
    'InSine', 'OutSine', 'InOutSine', 'InQuad', 'OutQuad', 'InOutQuad', 'InCubic', 'OutCubic',
    'InOutCubic', 'InQuart', 'OutQuart', 'InOutQuart', 'InQuint', 'OutQuint', 'InOutQuint',
    'InExpo', 'OutExpo', 'InOutExpo', 'InCirc', 'OutCirc', 'InOutCirc', 'InBack', 'OutBack',
    'InOutBack', 'InElastic', 'OutElastic', 'InOutElastic', 'InBounce', 'OutBounce', 'InOutBounce'
  }) do
    call('ease.'..name, math.ease[name], x)
  end
end

call('tau', function() return math.tau end)

local function list(n)
  local out = {}

  for i = 1, n do
    out[i] = rand(4) == 1 and str(3) or rand(50)
  end

  return out
end

local function numbers_list(n)
  local out = {}

  for i = 1, n do
    out[i] = rand(1000)
  end

  return out
end

local function map(n)
  local out = {}

  for _ = 1, n do
    out['k'..rand(20)] = rand(3) == 1 and { v = rand(9), name = str(3) } or rand(100)
  end

  return out
end

for _ = 1, 150 do
  local n = rand(8) - 1

  call('Copy', table.Copy, map(n))
  call('Copy nested', function()
    local t = { a = { b = { c = 1 } } }

    t.self = t
    t.a.up = t

    local c = table.Copy(t)

    return c.self == c, c.a.up == c, c.a.b.c, c ~= t
  end)
  call('IsEmpty', table.IsEmpty, map(n))
  call('Count', table.Count, map(n))
  call('HasValue', table.HasValue, list(n), rand(50))
  call('IsSequential', table.IsSequential, rand(2) == 1 and list(n) or map(n))
  call('Reverse', table.Reverse, list(n))
  call('Flip', table.Flip, numbers_list(n))
  call('KeysFromValue', function(t, v)
    local r = table.KeysFromValue(t, v)

    table.sort(r, function(a, b) return tostring(a) < tostring(b) end)

    return r
  end, map(n), rand(100))
  call('MemberValuesFromKey', function(t)
    local r = table.MemberValuesFromKey(t, 'v')

    table.sort(r)

    return r
  end, map(n))
  call('LowerKeyNames', table.LowerKeyNames, { ABC = 1, Def = { GhI = 2 }, [3] = 'x' })
  call('Inherit', table.Inherit, map(n), map(3))
  call('ForceInsert', table.ForceInsert, list(n), 'z')
  call('ForceInsert nil', table.ForceInsert, nil, 'z')
  call('Pack', table.Pack, 1, nil, 'x', nil)
  call('CollapseKeyValue', table.CollapseKeyValue, { { Key = 'a', Value = 1 }, { Key = 'b', Value = { { Key = 'c', Value = 2 } } } })
  call('Sanitise', table.Sanitise, { a = true, b = 1, c = 'x', d = Color(255, 1, 255, 255), e = { f = false } })
  call('DeSanitise', table.DeSanitise, table.Sanitise({ a = true, b = 1, c = 'x', d = Color(255, 1, 255, 255) }))
  call('GetWinningKey', table.GetWinningKey, table.Flip(numbers_list(n)))
  call('SortDesc', function(t) table.SortDesc(t) return t end, numbers_list(n))
  call('SortByKey', table.SortByKey, table.Flip(numbers_list(n)))
  call('SortByKey desc', table.SortByKey, table.Flip(numbers_list(n)), true)
  call('SortByMember', function(t, asc)
    table.SortByMember(t, 'v', asc)

    local out = {}

    for i, e in ipairs(t) do out[i] = e.v end

    return out
  end, { { v = rand(9) * 3 }, { v = rand(9) * 3 + 1 }, { v = 'Ab' }, { w = 1 }, { v = rand(9) * 3 + 2 } }, rand(2) == 1)
  call('SortByMember strings', function(t)
    table.SortByMember(t, 'name')

    local out = {}

    for i, e in ipairs(t) do out[i] = e.name end

    return out
  end, { { name = 'b' }, { name = 'A' }, { name = 'c' }, { name = 'D' } })
  call_mut('Merge', table.Merge, function() return { a = { x = 1 }, b = 2 }, { a = { y = 2 }, c = 3 } end)
  call_mut('Merge force', table.Merge, function() return { a = { x = 1 }, b = 2 }, { a = { y = 2 } }, true end)
  call_mut('Add', table.Add, function() return list(n), list(3) end)
  call_mut('Empty', table.Empty, function() return map(n) end)
  call_mut('CopyFromTo', function(from, to) table.CopyFromTo(from, to) return to end, function() return map(n), map(2) end)
  call_mut('RemoveByValue', table.RemoveByValue, function() return list(n), rand(50) end)
  call_mut('RemoveByValue map', table.RemoveByValue, function() return { a = 1, b = 2 }, 2 end)
  call_mut('ClearKeys', function(t, keep)
    local r = table.ClearKeys(t, keep)

    table.sort(r, function(a, b) return dump(a) < dump(b) end)

    return r
  end, function() return { a = { 1 }, b = { 2 } }, rand(2) == 1 end)
  call('KeyFromValue', table.KeyFromValue, list(n), rand(50))
  call('GetKeys', function(t) local k = table.GetKeys(t) table.sort(k, function(a, b) return tostring(a) < tostring(b) end) return k end, map(n))
  call('FindNext', table.FindNext, numbers_list(n), rand(1000))
  call('FindPrev', table.FindPrev, numbers_list(n + 1), rand(1000))
  call('ToString', table.ToString, list(n), rand(2) == 1 and 'name' or nil, rand(2) == 1)
  call('ToString nested', table.ToString, { 1, { 2, { 3 } }, 'x' }, 't', true)

  call('SortedPairs', function(t, desc)
    local out = {}

    for k, v in SortedPairs(t, desc) do
      out[#out + 1] = tostring(k)..'='..tostring(v)
    end

    return table.concat(out, ' ')
  end, rand(2) == 1 and map(n) or table.Flip(numbers_list(n)), rand(2) == 1)
  call('SortedPairs mixed', function()
    for _ in SortedPairs({ 1, a = 2 }) do end
  end)
  call('SortedPairsByValue', function(t, desc)
    local out = {}

    for k, v in SortedPairsByValue(t, desc) do
      out[#out + 1] = tostring(k)..'='..tostring(v)
    end

    return table.concat(out, ' ')
  end, table.Flip(table.Flip(numbers_list(n))), rand(2) == 1)
  call('SortedPairsByMemberValue', function(desc)
    local out = {}
    local t = { a = { m = 3 }, b = { m = 1 }, c = { m = 2 }, d = { m = 'x' } }

    t.d = nil

    for k, v in SortedPairsByMemberValue(t, 'm', desc) do
      out[#out + 1] = k..'='..v.m
    end

    return table.concat(out, ' ')
  end, rand(2) == 1)
end

local function bytes()
  local out = {}

  for i = 1, rand(6) - 1 do
    out[i] = pieces[rand(#pieces)]
  end

  return table.concat(out)
end

for _ = 1, 300 do
  local s = bytes()

  call('utf8.len', utf8.len, s)
  call('utf8.len range', utf8.len, s, rand(5) - 2, rand(5) - 2)
  call('utf8.codepoint', utf8.codepoint, s, rand(4), rand(6) - 1)
  call('utf8.offset', utf8.offset, s, rand(7) - 4, rand(2) == 1 and rand(5) - 2 or nil)
  call('utf8.force', utf8.force, s)
  call('utf8.GetChar', utf8.GetChar, s, rand(7) - 3)
  call('utf8.sub', utf8.sub, s, rand(7) - 3, rand(2) == 1 and rand(7) - 3 or nil)
  call('utf8.char', utf8.char, rand(200), rand(70000), rand(1114112) - 1)
  call('utf8.char bad', utf8.char, -rand(5))
  call('utf8.codes', function(x)
    local out = {}

    for p, c in utf8.codes(x) do
      out[#out + 1] = p..':'..c
    end

    return table.concat(out, ' ')
  end, s)
  call('utf8.match', function(x)
    local out = {}

    for c in x:gmatch(utf8.charpattern) do
      out[#out + 1] = c
    end

    return out
  end, s)
end

for _, s in ipairs({ 'camelCase', 'SNAKE_case', 'snake_case_name', 'MyNiceName', 'a1b', 'aB', 'ab', 'Ab', 'AB', 'a b', 'a_b', 'a__b', '_a', 'a_', 'a.b', 'a-b', 'a1', '1a', 'A1', 'hello world', 'HTTPServer', 'x', '', ' a', 'a ', 'aBC', 'abC1', 'test_Case', 'é', 'aé', 'a\255', 'a\tb', 'a\nb', 'abc_def_GHI' }) do
  call('cases NiceName', string.NiceName, s)
end

for c = 0, 255 do
  call('cases JavascriptSafe', string.JavascriptSafe, 'a'..string.char(c)..'b')
end

call('cases JavascriptSafe', string.JavascriptSafe, '\226\128\169')

for _, d in ipairs({ 1e-5, 2e-6, 1e-6, 5e-7, 1e-7, 5e-8, 2e-8, 1e-8, 9e-9, 5e-9, 1e-9, 1e-10, 0 }) do
  call('cases IsNearlyEqual', math.IsNearlyEqual, 1, 1 + d)
  call('cases IsNearlyEqual tol', math.IsNearlyEqual, 1, 1 + d, 1e-7)
end

local function single(f, ...)
  local v = f(...)

  return string.format('%.7g %.7g %.7g', v.x, v.y, v.z)
end

for i = 0, 5 do
  for k = 1, 4 do
    for _, t in ipairs({ 0, 0.1, 0.25, 0.3, 0.5, 0.6, 0.75, 0.9, 1, 1.25, 1.5 }) do
      call('cases calcBSplineN', math.calcBSplineN, i, k, t, 0.25)
    end
  end
end

for _, t in ipairs({ 0, 0.1, 0.5, 0.9, 1 }) do
  call('cases BSplinePoint', single, math.BSplinePoint, t, { Vector(1, 2, 3), Vector(5, 0, 1), Vector(2, 2, 2), Vector(8, 1, 0), Vector(3, 3, 9), Vector(9, 4, 1) }, 1)
  call('cases BSplinePoint 4', single, math.BSplinePoint, t, { Vector(1, 2, 3), Vector(5, 0, 1), Vector(2, 2, 2), Vector(8, 1, 0) }, 1)
end

call('cases Sanitise', table.Sanitise, { [1] = 5, [2] = 'x', n = 1.5, c1 = Color(0, 0, 0, 0), c2 = Color(10, 255, 3, 100), t = { true, false, 2 } })
call('cases DeSanitise', table.DeSanitise, { c = { __type = 'Color', r = '10' }, b = { __type = 'Bool', [1] = '1' }, n = '5', v = { 1, 2 } })

for _, list in ipairs({
  { { v = 3 }, { v = 1 }, { v = 2 } },
  { { v = 'b' }, { v = 'A' }, { v = 'c' }, { v = 'D' } },
  { { v = 3 }, { w = 1 }, { v = 2 } },
  { { v = 'b' }, { v = 2 } },
  { { v = true }, { v = false } }
}) do
  for _, asc in ipairs({ true, false }) do
    call('cases SortByMember', function(t, a)
      table.SortByMember(t, 'v', a)

      local out = {}

      for i, e in ipairs(t) do out[i] = tostring(e.v) end

      return table.concat(out, ' ')
    end, table.Copy(list), asc)
  end
end

for _, t in ipairs({ {}, { 1 }, { 1, 2 }, { a = 1 }, { 'x', { 'y' } }, { [1.5] = true }, { [true] = 'b' } }) do
  for _, name in ipairs({ false, 'n' }) do
    for _, nice in ipairs({ false, true }) do
      call('cases ToString', table.ToString, t, name or nil, nice)
    end
  end
end

for _, s in ipairs({ 'abc', 'aé€😀', '' }) do
  for i = -6, 6 do
    for _, j in ipairs({ -6, -2, -1, 0, 1, 2, 3, 6, 7, 20 }) do
      call('cases utf8.len', utf8.len, s, i, j)
      call('cases utf8.codepoint', utf8.codepoint, s, i, j)
      call('cases utf8.sub', utf8.sub, s, i, j)
    end

    call('cases utf8.sub1', utf8.sub, s, i)
    call('cases utf8.GetChar', utf8.GetChar, s, i)

    for n = -4, 4 do
      call('cases utf8.offset', utf8.offset, s, n, i)
    end

    call('cases utf8.offset nil', utf8.offset, s, i)
  end
end

for _, args in ipairs({ { 'xxaxx', 'x' }, { 'abab', 'ab' }, { 'abba', 'ab' }, { 'aab', 'ab' }, { 'ba', 'ab' }, { 'a.a', '.' }, { '..x..', '.' }, { '  x  ' }, { 'x\0', '\0' } }) do
  call('cases Trim', string.Trim, args[1], args[2])
  call('cases TrimLeft', string.TrimLeft, args[1], args[2])
  call('cases TrimRight', string.TrimRight, args[1], args[2])
end

for _, list in ipairs({ { 1, 2, 1, 3 }, { 5 }, {} }) do
  for _, v in ipairs({ 1, 3, 5, 9 }) do
    call('cases FindNext', table.FindNext, list, v)
    if #list > 0 then call('cases FindPrev', table.FindPrev, list, v) end
  end
end

for _, list in ipairs({
  { { v = true }, { v = false }, { v = true } },
  { { v = 1 }, { v = true } },
  { { v = 'a' }, { v = 'A' }, { v = 'b' } },
  { { v = '10' }, { v = '9' }, { v = '1' } },
  { { w = 1 }, { v = 1 }, { w = 2 }, { v = 2 } },
  { { v = 'b' }, { w = 1 }, { v = 'a' } },
  { { v = false }, { v = 2 }, { v = 1 } },
  { { v = 1 }, { v = 1 }, { v = 1 }, { v = 1 }, { v = 1 } },
  { { v = 2 }, { v = 'B' } },
  { { v = 'B' }, { v = 2 } }
}) do
  for _, asc in ipairs({ true, false }) do
    call('cases SortByMember2', function(t, a)
      table.SortByMember(t, 'v', a)

      local out = {}

      for i, e in ipairs(t) do out[i] = tostring(e.v) end

      return table.concat(out, ' ')
    end, table.Copy(list), asc)
  end
end

for _, t in ipairs({ { a = { b = { c = 1 } } }, { { { 1 } } }, { 'q"\\n\n\0' }, { ['a b'] = 1 }, { [5] = 1 }, { -1.5 }, { 0.1 }, { 1e20 }, { a = { 1 } }, { { a = 1 } }, { 'it\'s' } }) do
  for _, nice in ipairs({ false, true }) do
    call('cases ToString2', table.ToString, t, 'n', nice)
  end
end

for _, args in ipairs({ { 0x110000 }, { 0x7FFFFFFF }, { 65.5 }, { '66' }, { 'x' }, {}, { 0 } }) do
  call('cases utf8.char', utf8.char, unpack(args))
end

for _, t in ipairs({ { 1, 2, a = 3 }, { [1] = 1, [3] = 3 }, { [2] = 1 } }) do
  call('cases ToString3', table.ToString, t, nil, true)
end

local function pack(...)
  return { n = select('#', ...), ... }
end

local function with_clock(now, f, ...)
  local cur, sys, real = CurTime, SysTime, RealTime

  CurTime = function() return now end
  SysTime = function() return now + 1000 end
  RealTime = function() return now + 2000 end

  local results = pack(pcall(f, ...))

  CurTime, SysTime, RealTime = cur, sys, real

  if not results[1] then error(results[2], 0) end

  return unpack(results, 2, results.n)
end

local function console_of(f, ...)
  local out = {}
  local msg, msgn, prt = Msg, MsgN, print

  local function joined(separator, ...)
    local parts = {}

    for i = 1, select('#', ...) do parts[i] = tostring((select(i, ...))) end

    return table.concat(parts, separator)
  end

  Msg = function(...) out[#out + 1] = joined('', ...) end
  MsgN = function(...) out[#out + 1] = joined('', ...)..'\n' end
  print = function(...) out[#out + 1] = joined('\t', ...)..'\n' end

  local results = pack(pcall(f, ...))

  Msg, MsgN, print = msg, msgn, prt

  if not results[1] then error(results[2], 0) end

  return (table.concat(out):gsub('table: 0x%x+', 'table'))
end

local color_specs = {
  { 255, 0, 0 }, { 255, 0, 0, 128 }, { 300, -5, 12.5, 999 }, { '12', '34', '56' }, { 0, 0, 0, 0 },
  { 1.5, 2.5, 3.5 }, { 255, 255, 255 }, { 128, 64, 32, 16 }, { 10, 200, 90 }, { 17, 17, 17, 255 },
  { 0, 128, 255 }, { 254.6, 0.4, 100 }
}

for _, spec in ipairs(color_specs) do
  call('Color', function(...)
    local c = Color(...)

    return c, IsColor(c), tostring(c), c == Color(...), c == Color(0, 0, 0, 1)
  end, unpack(spec))

  local function on(method, ...)
    call('Color:'..method, function(args, ...)
      local c = Color(unpack(args))
      local results = pack(c[method](c, ...))

      return c, results
    end, spec, ...)
  end

  for _, method in ipairs({ 'Copy', 'Unpack', 'ToTable', 'ToVector', 'ToHSV', 'ToHSL', 'ToHWB', 'GetHue', 'GetSaturation', 'GetBrightness', 'GetLightness', 'GetWhiteness', 'GetBlackness' }) do
    on(method)
  end

  on('ToHex')
  on('ToHex', true)
  on('ToHex', false)

  for _, amount in ipairs({ 0, 0.25, 0.5, 1, 30, 180, 400, -10 }) do
    for _, part in ipairs({ 'Hue', 'Saturation', 'Brightness', 'Lightness', 'Whiteness', 'Blackness' }) do
      on('Set'..part, amount)
      on('Add'..part, amount)
    end
  end

  for _, f in ipairs({ 0, 0.25, 0.5, 1, 1.5, -0.5 }) do
    on('Lerp', Color(0, 100, 200, 50), f)
  end

  on('SetUnpacked', 1, 2, 3, 4)
  on('SetUnpacked', 1, 2)
  call('ColorAlpha', function(args, a) return ColorAlpha(Color(unpack(args)), a) end, spec, 77)
  call('ColorToHSV', function(args) return ColorToHSV(Color(unpack(args))) end, spec)
  call('ColorToHSL', function(args) return ColorToHSL(Color(unpack(args))) end, spec)
end

call('ColorAlpha table', ColorAlpha, { r = 1, g = 2, b = 3, a = 4 }, 9)
call('IsColor', function() return IsColor({ r = 1, g = 2, b = 3, a = 4 }), IsColor(nil), IsColor(5), IsColor('x') end)
call('color globals', function() return color_white, color_black, color_transparent end)

for _, op in ipairs({
  function(a, b) return a + b end, function(a, b) return a - b end, function(a, b) return a * b end,
  function(a, b) return a / b end, function(a) return -a end, function(a, b) return a < b end,
  function(a, b) return a <= b end, function(a, b) return a..b end, function(a) return #a end,
  function(a) return a * 2 end, function(a) return a / 2 end
}) do
  call('Color operator', op, Color(10, 20, 30, 40), Color(1, 2, 3, 4))
end

for h = 0, 360, 30 do
  for _, a in ipairs({ 0, 0.3, 0.5, 1 }) do
    for _, b in ipairs({ 0, 0.25, 0.6, 1 }) do
      call('HSVToColor', HSVToColor, h, a, b)
      call('HSLToColor', HSLToColor, h, a, b)
      call('HWBToColor', HWBToColor, h, a, b)
    end
  end
end

for _, args in ipairs({ { 0, 1.5, 1 }, { 0, 0.5, -1 }, { 0, -0.5, 0.5 }, { 0, 0.5, 2 }, { 30, 2, 2 }, { 400, 1, 1 }, { -30, 1, 1 } }) do
  call('HSVToColor range', HSVToColor, unpack(args))
  call('HSLToColor range', HSLToColor, unpack(args))
  call('HWBToColor range', HWBToColor, unpack(args))
end

call('NaN clamps', function()
  local nan = 0 / 0

  return math.max(nan, 0), math.max(0, nan), math.min(nan, 255), math.min(255, nan), math.floor(nan), math.Clamp(nan, 0, 255)
end)

for _, hex in ipairs({ '#fff', 'fff', '#FF0000', 'ff000080', '#12345678', '#1234', 'zz', '', '#12345', '#abcdef', 12 }) do
  call('HexToColor', HexToColor, hex)
end

local tobool_inputs = { true, false, 'true', 'false', '0', '1', 0, 1, -1, '', 'yes', 'FALSE', ' 0', '0.0', 2, n = 15 }

for i = 1, tobool_inputs.n do
  call('tobool', tobool, tobool_inputs[i])
  call('util.tobool', util.tobool, tobool_inputs[i])
end

call('tobool', tobool, nil)
call('tobool', tobool, {})

for _, v in ipairs({ false, true, 'str', {}, { IsValid = function() return true end }, { IsValid = function() return nil end }, { IsValid = function() return 'yes' end }, 0 }) do
  call('IsValid', IsValid, v)
end

call('IsValid', IsValid, nil)
call('Format', Format, '%d-%s-%5.2f', 5, 'x', 1.5)

for _, cond in ipairs({ true, false, 0, '' }) do
  call('Either', Either, cond, 'a', 'b')
end

call('Either', Either, nil, 'a', 'b')

for _, f in ipairs({ -1, 0, 0.1, 0.3, 0.5, 0.7, 1, 1.5 }) do
  call('Lerp', Lerp, f, 0, 10)
  call('Lerp', Lerp, f, 0.1, 0.3)
  call('Lerp', Lerp, f, 255, -3)
end

for n = -3, 130 do
  call('STNDRD', STNDRD, n)
end

call('STNDRD', STNDRD, 1.5)

for _, args in ipairs({ { 1, 0, 1, 0 }, { 0.5, 3, 5, 0 }, { 2, -1, 1, 0.25 }, { 0.1, 10, 20, 3 } }) do
  for _, now in ipairs({ 0, 1.25, 12.5, 100.1 }) do
    call('TimedSin', function(t, ...) return with_clock(t, TimedSin, ...) end, now, unpack(args))
    call('TimedCos', function(t, ...) return with_clock(t, TimedCos, ...) end, now, unpack(args))
  end
end

for _, t in ipairs({ {}, { 1, 2, 3 }, { a = 1, b = { c = 2, d = { e = 'x' } }, [5] = true }, { 'test', 3829.4, { 'foo', 'baah', 20 / 5 }, true },
  { [10] = 1, [9] = 2, Z = 4, a = 5 }, { ['q"\n'] = 1 }, { [true] = 1 }, { [1.5] = 2, [-1] = 3 } }) do
  call('PrintTable', console_of, PrintTable, t)
  call('PrintTable indent', console_of, PrintTable, t, 2)
end

call('PrintTable cycle', console_of, function()
  local t = { a = 1 }

  t.self = t
  PrintTable(t)
end)

call('FORCE', function() return FORCE_STRING, FORCE_NUMBER, FORCE_BOOL, FORCE_ANGLE, FORCE_COLOR, FORCE_VECTOR end)

for _, force in ipairs({ false, FORCE_STRING, FORCE_NUMBER, FORCE_BOOL }) do
  for _, v in ipairs({ '5', 5, '0', 'x', true, false }) do
    call('AccessorFunc', function(f, value)
      local t = {}

      AccessorFunc(t, 'k', 'Thing', f or nil)

      local set = pack(t:SetThing(value))

      return t:GetThing(), t.k, set, t.GetThing ~= nil, t.SetThing ~= nil
    end, force, v)
  end
end

local stack = util.Stack()

for _, operation in ipairs({
  { 'Size' }, { 'Top' }, { 'Pop' }, { 'PopMulti' }, { 'Push', 10 }, { 'Push', 20 }, { 'Push', 30 }, { 'Push', 40 },
  { 'Push', 50 }, { 'Size' }, { 'Top' }, { 'Pop' }, { 'Pop', 2 }, { 'Size' }, { 'Top' }, { 'Push', 'x' }, { 'Push', false },
  { 'PopMulti', 2 }, { 'Size' }, { 'PopMulti', 5 }, { 'Size' }, { 'Push', 1 }, { 'Push', 2 }, { 'Pop', 0 }, { 'PopMulti', 0 },
  { 'Pop', -1 }, { 'Size' }, { 'Pop', 3 }, { 'Size' }, { 'Top' }, { 'Push', nil }, { 'Size' }, { 'Top' }
}) do
  call('util.Stack '..operation[1], function(name, ...) return pack(stack[name](stack, ...)), stack:Size() end, unpack(operation))
end

for _, n in ipairs({ 0, 1, 1.5, -1.5, 0.1, 1 / 3, 1e20, 123.456, 100, -0.001, 1e-7 }) do
  call('util.NiceFloat', util.NiceFloat, n)
  call('util.TypeToString', util.TypeToString, n)
end

for _, v in ipairs({ true, false, 'str', Color(1, 2, 3, 4), Vector(1.5, 2, 3), Angle(4, 5.5, 6) }) do
  call('util.TypeToString', util.TypeToString, v)
end

for _, args in ipairs({
  { '5', 'int' }, { '5.5', 'int' }, { '5.5', 'float' }, { '5.5', 'number' }, { 'x', 'string' }, { '1', 'bool' },
  { '0', 'boolean' }, { 'true', 'bool' }, { '1 2 3 4', 'color' }, { '1 2 3', 'vector' }, { '1 2 3', 'angle' },
  { '5', 'Int' }, { '5', 'nope' }, { 'abc', 'number' }
}) do
  call('util.StringToType', util.StringToType, unpack(args))
end

call('util.Timer', function()
  return with_clock(10, function()
    local t = util.Timer(5)
    local out = { pack(t:Started(), t:Elapsed(), t:GetElaspedTime()) }

    CurTime = function() return 16 end
    out[#out + 1] = pack(t:Started(), t:Elapsed(), t:GetElaspedTime())
    SysTime = function() return 1016 end
    RealTime = function() return 2016 end
    out[#out + 1] = pack(t:Started(), t:Elapsed(), t:GetElaspedTime())
    t:Reset()
    out[#out + 1] = pack(t:Started(), t:Elapsed(), t:GetElaspedTime())
    t:Start(2)
    out[#out + 1] = pack(t:Started(), t:Elapsed(), t:GetElaspedTime())
    t:Start()
    out[#out + 1] = pack(t:Started(), t:Elapsed(), t:GetElaspedTime())

    return out, util.Timer():Elapsed()
  end)
end)

for _, name in ipairs({ 'nonexistent', 'file', '../x', '', 'a.b', 'mysqloo' }) do
  call('util.IsBinaryModuleInstalled', util.IsBinaryModuleInstalled, name)
end

call('util.IsBinaryModuleInstalled', util.IsBinaryModuleInstalled, 5)

call('IsTableOfEntitiesValid', function()
  local valid = { IsValid = function() return true end }
  local invalid = { IsValid = function() return false end }

  return IsTableOfEntitiesValid({}), IsTableOfEntitiesValid({ valid, valid }), IsTableOfEntitiesValid({ valid, invalid }), IsTableOfEntitiesValid(nil)
end)

call('RandomPairs', function()
  local seen = {}

  for k, v in RandomPairs({ a = 1, b = 2, c = 3, 4 }) do seen[#seen + 1] = tostring(k)..'='..tostring(v) end

  table.sort(seen)

  return seen
end)

local log = {}

local function logged(name, ...)
  local returns = pack(...)

  return named(name, function(...)
    log[#log + 1] = name..dump(pack(...))

    return unpack(returns, 1, returns.n)
  end)
end

local function step(name, f, ...)
  call('hook '..name, function(...)
    log = {}

    local results = pack(f(...))

    table.sort(log)

    return results, log
  end, ...)
end

local gm = { HookFallback = logged('gm.HookFallback', 'gm says') }

step('Add plain', function()
  hook.Add('HookFallback', 'first', logged('first'))
  hook.Add('HookFallback', 'second', logged('second'))

  return hook.Call('HookFallback', gm, 1, 'two')
end)
step('Call nil gamemode', hook.Call, 'HookFallback', nil, 3)
step('Call missing event', hook.Call, 'NoSuchEvent', gm, 3)
step('Call missing event nil gamemode', hook.Call, 'NoSuchEvent', nil)
step('Call gamemode without event', hook.Call, 'HookFallback', {}, 4)
step('Add returning', function()
  hook.Add('HookReturn', 'only', logged('only', 1, 2, 3, 4, 5, 6, 7, 8))

  return hook.Call('HookReturn', gm, 'x')
end)
step('Add false', function()
  hook.Add('HookFalse', 'f', logged('f', false, 'more'))

  return hook.Call('HookFalse', { HookFalse = logged('gm.HookFalse', 'gm') })
end)
step('Add nil then value', function()
  hook.Add('HookNilFirst', 'n', logged('n', nil, 'second value'))

  return hook.Call('HookNilFirst', { HookNilFirst = logged('gm.HookNilFirst', 'gm', 'two') }, 9)
end)
step('Replace', function()
  hook.Add('HookFallback', 'first', logged('first replaced'))

  return hook.Call('HookFallback', gm, 5)
end)
step('Remove', function()
  hook.Remove('HookFallback', 'second')

  return hook.Call('HookFallback', gm, 6)
end)
step('Remove missing', function()
  hook.Remove('HookFallback', 'never')
  hook.Remove('NeverAdded', 'never')

  return hook.Call('HookFallback', gm, 7)
end)

local valid_object = { valid = true }

function valid_object:IsValid() return self.valid end

step('Add object', function()
  hook.Add('HookObject', valid_object, logged('object', 'from object'))

  return hook.Call('HookObject', nil, 'arg')
end)
step('Object invalid', function()
  valid_object.valid = false

  local results = pack(hook.Call('HookObject', { HookObject = logged('gm.HookObject', 'gm') }, 'arg'))

  return results, hook.GetTable().HookObject and hook.GetTable().HookObject[valid_object] ~= nil
end)
step('Add table without IsValid', function()
  local plain = {}

  hook.Add('HookPlainTable', plain, logged('plain', 'ran'))

  local results = pack(hook.Call('HookPlainTable', nil, 1))

  return results, hook.GetTable().HookPlainTable[plain] ~= nil
end)

for _, id in ipairs({ 5, true, false }) do
  step('Add bad identifier', function(identifier) hook.Add('HookBad', identifier, logged('bad')) return hook.GetTable().HookBad end, id)
end

step('Add nil identifier', function() hook.Add('HookBad', nil, logged('bad')) return hook.GetTable().HookBad end)
step('Add non-function', function() hook.Add('HookBad', 'x', 'not a function') return hook.GetTable().HookBad end)
step('Add non-string event', function() hook.Add(5, 'x', logged('bad')) return hook.GetTable()[5] end)
step('Call non-string event', hook.Call, 5, gm)
step('Run without gamemode', function() return hook.Run('HookFallback', 'run') end)
step('Run', function()
  local saved = GAMEMODE

  GAMEMODE = { HookFallback = logged('GAMEMODE.HookFallback', 'from GAMEMODE') }

  local results = pack(hook.Run('HookFallback', 'run'))

  GAMEMODE = saved

  return results
end)
step('Remove itself while called', function()
  local second_call = false

  hook.Add('HookSelf', 'self', named('self remover', function()
    log[#log + 1] = 'self'
    hook.Remove('HookSelf', 'self')
    hook.Add('HookSelf', 'added', named('added later', function()
      if second_call then log[#log + 1] = 'added later' end
    end))
  end))

  local first = pack(hook.Call('HookSelf', { HookSelf = logged('gm.HookSelf', 'gm') }))

  second_call = true

  local second = pack(hook.Call('HookSelf', { HookSelf = logged('gm.HookSelf', 'gm') }))

  return first, second, hook.GetTable().HookSelf
end)
step('Error in hook', function()
  hook.Add('HookError', 'e', function() error('in a hook', 0) end)

  return hook.Call('HookError', gm)
end)
step('GetTable', function()
  local t = hook.GetTable()
  local out = {}

  for event, hooks in pairs(t) do
    if type(event) == 'string' and event:sub(1, 4) == 'Hook' then out[event] = hooks end
  end

  return out
end)

for _, name in ipairs({ 'HookFallback', 'HookReturn', 'HookFalse', 'HookNilFirst', 'HookObject', 'HookPlainTable', 'HookSelf', 'HookError', 'HookBad' }) do
  for id in pairs(table.Copy(hook.GetTable()[name] or {})) do
    hook.Remove(name, id)
  end
end

local received = {}
local command = named('command', function(actor, cmd, args, args_text)
  received[#received + 1] = { tostring(actor), cmd, args, args_text }
end)
local complete = named('complete', function(cmd, args_text, args)
  return { cmd..' one', cmd..' '..tostring(args_text), args and #args }
end)

local function console_step(name, f, ...)
  call('concommand '..name, function(...)
    received = {}

    local results = pack(f(...))

    return results, received
  end, ...)
end

console_step('Add', function()
  concommand.Add('FpkgCompare', command, complete, 'help text', FCVAR_NONE)
  concommand.Add('fpkg_compare_plain', command)

  local commands, completions = concommand.GetTable()

  return commands.fpkgcompare ~= nil, commands.FpkgCompare ~= nil, commands.fpkg_compare_plain ~= nil, completions.fpkgcompare ~= nil, completions.fpkg_compare_plain ~= nil
end)

for _, name in ipairs({ 'fpkgcompare', 'FPKGCOMPARE', 'FpkgCompare', 'nope' }) do
  console_step('Run', concommand.Run, nil, name, { 'a', 'b c' }, 'a "b c"')
end

console_step('Run string args', concommand.Run, nil, 'fpkgcompare', 'a b', 'a b')

for _, args in ipairs({ { 'fpkgcompare', ' x y' }, { 'FPKGCOMPARE', '' }, { 'fpkg_compare_plain', 'x' }, { 'nope', 'x' } }) do
  console_step('AutoComplete', concommand.AutoComplete, unpack(args))
end

for _, line in ipairs({ 'fpkgcompare', '  fpkgcompare a "b c"', 'FpkgCompare   x  y  ', 'fpkgcompare a;fpkgcompare b', 'fpkgcompare "quoted ; semicolon"', '"fpkgcompare" q', 'fpkgcompare\ta\tb', '' }) do
  console_step('line', concommand.__fpkg_run_line, line)
end

console_step('Remove', function()
  concommand.Remove('FPKGCOMPARE')

  local commands = concommand.GetTable()

  return commands.fpkgcompare ~= nil, concommand.Run(nil, 'fpkgcompare', {}, '')
end)
console_step('Add bad', function() concommand.Add(5, command) return concommand.GetTable()[5] end)
console_step('Add non-function', function() concommand.Add('fpkg_compare_bad', 'x') return concommand.GetTable().fpkg_compare_bad end)

local convar = CreateConVar('fpkg_compare', '5', FCVAR_ARCHIVE, 'help', 0, 10)
local changes = {}

local function changed(name)
  return named(name, function(cvar, old, new)
    changes[#changes + 1] = name..':'..tostring(cvar)..':'..tostring(old)..'>'..tostring(new)
  end)
end

local function cvars_step(name, f, ...)
  call('cvars '..name, function(...)
    changes = {}

    local results = pack(f(...))

    table.sort(changes)

    return results, changes
  end, ...)
end

cvars_step('callbacks', function()
  cvars.AddChangeCallback('fpkg_compare', changed('one'), 'one')
  cvars.AddChangeCallback('fpkg_compare', changed('anonymous'))
  convar:SetString('7')

  return cvars.GetConVarCallbacks('fpkg_compare')
end)
cvars_step('same identifier', function()
  cvars.AddChangeCallback('fpkg_compare', changed('one again'), 'one')
  convar:SetString('8')

  return cvars.GetConVarCallbacks('fpkg_compare')
end)
cvars_step('remove', function()
  cvars.RemoveChangeCallback('fpkg_compare', 'one')
  convar:SetString('9')

  return cvars.GetConVarCallbacks('fpkg_compare')
end)
cvars_step('unchanged value', function() convar:SetString('9') return true end)
cvars_step('missing convar', function() return cvars.GetConVarCallbacks('fpkg_compare_none'), cvars.GetConVarCallbacks('fpkg_compare_none', true), cvars.GetConVarCallbacks('fpkg_compare_none') end)
cvars_step('OnConVarChanged', cvars.OnConVarChanged, 'fpkg_compare', 'a', 'b')
cvars_step('OnConVarChanged unknown', cvars.OnConVarChanged, 'fpkg_compare_unknown', 'a', 'b')
cvars_step('remove missing', function() cvars.RemoveChangeCallback('fpkg_compare_unknown', 'x') cvars.RemoveChangeCallback('fpkg_compare', 'never') return true end)

for _, value in ipairs({ '0', '1', 'abc', '2.5', '', '10', '-3', ' 4', '0.5', '0.0', '5x', '1e3', '0x10', '-0.5', 'true' }) do
  cvars_step('read', function(v)
    local plain = CreateConVar('fpkg_compare_read', '1')

    plain:SetString(v)

    if plain:GetString() ~= v then error('the convar did not take the value', 0) end

    return pack(cvars.Bool('fpkg_compare_read')), pack(cvars.Bool('fpkg_compare_read', true)), pack(cvars.Number('fpkg_compare_read')),
      pack(cvars.Number('fpkg_compare_read', 42)), pack(cvars.String('fpkg_compare_read')), pack(cvars.String('fpkg_compare_read', 'd'))
  end, value)
end

cvars_step('read missing', function()
  return pack(cvars.Bool('fpkg_compare_missing')), pack(cvars.Bool('fpkg_compare_missing', true)), pack(cvars.Number('fpkg_compare_missing', 3)),
    pack(cvars.String('fpkg_compare_missing', 'd')), pack(cvars.String('fpkg_compare_missing'))
end)

for _, name in ipairs({ 'npc_antlion', 'npc_antlionguard', 'npc_antlion_worker', 'npc_barnacle', 'npc_breen', 'npc_clawscanner', 'npc_combine_s', 'npc_combinedropship', 'npc_combinegunship', 'npc_crow', 'npc_cscanner', 'npc_dog', 'npc_eli', 'npc_fastzombie', 'npc_fastzombie_torso', 'npc_headcrab', 'npc_headcrab_black', 'npc_headcrab_fast', 'npc_headcrab_poison', 'npc_helicopter', 'npc_hunter', 'npc_ichthyosaur', 'npc_kleiner', 'npc_manhack', 'npc_metropolice', 'npc_monk', 'npc_mossman', 'npc_pigeon', 'npc_poisonzombie', 'npc_rollermine', 'npc_seagull', 'npc_sniper', 'npc_stalker', 'npc_strider', 'npc_turret_ceiling', 'npc_turret_floor', 'npc_vortigaunt', 'npc_zombie', 'npc_zombie_torso', 'npc_zombine', 'npc_alyx', 'npc_barney', 'npc_citizen', 'npc_gman', 'npc_magnusson', 'npc_fisherman', 'npc_odessa', 'npc_grigori', 'npc_combine_camera', 'npc_turret_ground', 'npc_apcdriver', 'npc_bullseye', 'npc_enemyfinder', 'npc_missiledefense', 'npc_launcher', 'npc_vortigaunt_slave', 'npc_monk_x', 'player', 'monster_alien_grunt', 'monster_zombie', 'monster_scientist', 'monster_barney', 'monster_headcrab', 'monster_houndeye', 'monster_bullchicken', 'monster_alien_slave', 'monster_human_grunt', 'monster_gargantua', 'monster_tentacle', 'monster_snark', 'monster_babycrab', 'monster_bigmomma', 'monster_ichthyosaur', 'monster_leech', 'monster_nihilanth', 'monster_cockroach', 'monster_gman', 'monster_human_assassin', 'monster_alien_controller', 'monster_sentry', 'monster_turret', 'monster_miniturret', 'monster_apache', 'monster_osprey', '', 'NPC_ZOMBIE', 'npc_zombie ' }) do
  call('cases entity names', function(n) return IsEnemyEntityName(n), IsFriendEntityName(n) end, name)
end

call('cases Add_NPC_Class', function()
  NUM_AI_CLASSES = 30

  local before = CLASS_FPKG_COMPARE

  Add_NPC_Class('CLASS_FPKG_COMPARE')
  Add_NPC_Class('CLASS_FPKG_COMPARE_TWO')

  return before, CLASS_FPKG_COMPARE, CLASS_FPKG_COMPARE_TWO, NUM_AI_CLASSES
end)

local function registry_step(name, f, ...)
  call('registry '..name, function(...)
    log = {}

    local results = pack(f(...))

    table.sort(log)

    return results, log
  end, ...)
end

registry_step('baseclass unknown', function()
  local early = baseclass.Get('fpkg_compare_base')

  return early, baseclass.Get('fpkg_compare_base') == early
end)
registry_step('baseclass Set', function()
  local early = baseclass.Get('fpkg_compare_base')
  local meta = { __index = { inherited = 'yes' } }

  baseclass.Set('fpkg_compare_base', setmetatable({ A = 1, Think = named('Think', function() end) }, meta))

  local now = baseclass.Get('fpkg_compare_base')

  return now, now == early, now.inherited, getmetatable(now) == meta
end)
registry_step('baseclass Set again', function()
  local before = baseclass.Get('fpkg_compare_base')

  baseclass.Set('fpkg_compare_base', { B = 2 })

  local now = baseclass.Get('fpkg_compare_base')

  return now, now == before
end)
registry_step('baseclass Set new', function()
  local tab = { C = 3 }

  baseclass.Set('fpkg_compare_new', tab)

  return baseclass.Get('fpkg_compare_new') == tab, baseclass.Get('fpkg_compare_new')
end)

local swep_base = { PrintName = 'Base', Primary = { Ammo = 'none', ClipSize = -1 }, Think = named('base Think', function() end), Shared = { a = 1 } }
local swep = { Base = 'fpkg_weapon_base', PrintName = 'Child', Primary = { ClipSize = 8 }, Shared = { b = 2 } }

registry_step('weapons Register', function()
  weapons.Register(swep_base, 'fpkg_weapon_base')
  weapons.Register(swep, 'fpkg_weapon')

  return swep.ClassName, swep_base.ClassName, weapons.GetStored('fpkg_weapon') == swep, weapons.GetStored('fpkg_weapon_none')
end)
registry_step('weapons Get', function()
  local copy = weapons.Get('fpkg_weapon')

  return copy, copy ~= swep, copy.Primary ~= swep.Primary, copy.BaseClass == weapons.GetStored('fpkg_weapon_base'), copy.Primary.BaseClass ~= nil
end)
registry_step('weapons Get base', function() return weapons.Get('fpkg_weapon_base') end)
registry_step('weapons Get missing', function() return weapons.Get('fpkg_weapon_none') end)
registry_step('weapons Get into', function()
  local into = { Extra = true, PrintName = 'kept?' }
  local result = weapons.Get('fpkg_weapon', into)

  return result == into, into
end)
registry_step('weapons IsBasedOn', function()
  return weapons.IsBasedOn('fpkg_weapon', 'fpkg_weapon_base'), weapons.IsBasedOn('fpkg_weapon', 'weapon_base'), weapons.IsBasedOn('fpkg_weapon_base', 'fpkg_weapon'), weapons.IsBasedOn('fpkg_weapon', 'fpkg_weapon'), weapons.IsBasedOn('fpkg_weapon_none', 'x')
end)
registry_step('weapons GetList', function()
  local found = {}

  for _, entry in pairs(weapons.GetList()) do
    if type(entry) == 'table' and type(entry.ClassName) == 'string' and entry.ClassName:sub(1, 4) == 'fpkg' then found[#found + 1] = entry.ClassName end
  end

  table.sort(found)

  return found
end)
registry_step('weapons baseclass', function() return baseclass.Get('fpkg_weapon') == swep, baseclass.Get('fpkg_weapon').PrintName end)
registry_step('weapons Register again', function()
  weapons.Register({ Base = 'fpkg_weapon_base', PrintName = 'Again' }, 'fpkg_weapon')

  return weapons.GetStored('fpkg_weapon'), weapons.GetStored('fpkg_weapon') == swep, weapons.Get('fpkg_weapon').PrintName, baseclass.Get('fpkg_weapon').PrintName
end)
registry_step('weapons PreRegisterSWEP', function()
  hook.Add('PreRegisterSWEP', 'refuse', function(t, class) if class == 'fpkg_weapon_refused' then return false end end)
  weapons.Register({ PrintName = 'Refused' }, 'fpkg_weapon_refused')
  hook.Remove('PreRegisterSWEP', 'refuse')

  return weapons.GetStored('fpkg_weapon_refused')
end)

local ent_base = { Type = 'anim', PrintName = 'Base entity', Spawnable = false, Draw = named('Draw', function() end), Data = { x = 1 } }
local ent = { Base = 'fpkg_ent_base', PrintName = 'Child entity', Spawnable = true, Data = { y = 2 } }

registry_step('scripted_ents Register', function()
  scripted_ents.Register(ent_base, 'fpkg_ent_base')
  scripted_ents.Register(ent, 'fpkg_ent')

  return ent.ClassName, scripted_ents.GetStored('fpkg_ent'), scripted_ents.GetStored('fpkg_ent').t == ent, scripted_ents.GetStored('fpkg_ent_none')
end)
registry_step('scripted_ents Get', function()
  local copy = scripted_ents.Get('fpkg_ent')

  return copy, copy ~= ent, copy.BaseClass ~= nil
end)
registry_step('scripted_ents Get missing', function() return scripted_ents.Get('fpkg_ent_none') end)
registry_step('scripted_ents GetType', function() return scripted_ents.GetType('fpkg_ent'), scripted_ents.GetType('fpkg_ent_base'), scripted_ents.GetType('fpkg_ent_none') end)
registry_step('scripted_ents GetMember', function() return scripted_ents.GetMember('fpkg_ent', 'PrintName'), scripted_ents.GetMember('fpkg_ent', 'Type'), scripted_ents.GetMember('fpkg_ent', 'Draw'), scripted_ents.GetMember('fpkg_ent', 'Nothing'), scripted_ents.GetMember('fpkg_ent_none', 'Type') end)
registry_step('scripted_ents IsBasedOn', function() return scripted_ents.IsBasedOn('fpkg_ent', 'fpkg_ent_base'), scripted_ents.IsBasedOn('fpkg_ent_base', 'fpkg_ent'), scripted_ents.IsBasedOn('fpkg_ent_none', 'x') end)
registry_step('scripted_ents Alias', function()
  scripted_ents.Alias('fpkg_ent_alias', 'fpkg_ent')

  return scripted_ents.Get('fpkg_ent_alias'), scripted_ents.GetStored('fpkg_ent_alias'), scripted_ents.GetType('fpkg_ent_alias')
end)
registry_step('scripted_ents GetList', function()
  local found = {}

  for class, entry in pairs(scripted_ents.GetList()) do
    if type(class) == 'string' and class:sub(1, 4) == 'fpkg' then found[class] = entry end
  end

  return found
end)
registry_step('scripted_ents GetSpawnable', function()
  local found = {}

  for _, entry in pairs(scripted_ents.GetSpawnable()) do
    if type(entry) == 'table' and type(entry.ClassName) == 'string' and entry.ClassName:sub(1, 4) == 'fpkg' then found[#found + 1] = entry end
  end

  table.sort(found, function(a, b) return a.ClassName < b.ClassName end)

  return found
end)

for _, kind in ipairs({ 'point', 'brush', 'ai', 'nextbot', 'filter', 'weird', false }) do
  registry_step('scripted_ents default Base', function(k)
    local class = 'fpkg_ent_kind_'..tostring(k)

    scripted_ents.Register({ Type = k or nil }, class)

    local entry = scripted_ents.GetList()[class]

    return entry.Base, entry.type, entry.t.Base, scripted_ents.GetType(class)
  end, kind)
end

registry_step('weapons chain', function()
  weapons.Register({ PrintName = 'A' }, 'fpkg_chain_a')
  weapons.Register({ Base = 'fpkg_chain_a' }, 'fpkg_chain_b')
  weapons.Register({ Base = 'fpkg_chain_b' }, 'fpkg_chain_c')
  weapons.Register({ Base = 'fpkg_chain_missing' }, 'fpkg_chain_d')

  return weapons.IsBasedOn('fpkg_chain_c', 'fpkg_chain_a'), weapons.IsBasedOn('fpkg_chain_c', 'fpkg_chain_b'), weapons.IsBasedOn('fpkg_chain_b', 'fpkg_chain_a'),
    weapons.IsBasedOn('fpkg_chain_a', 'weapon_base'), weapons.IsBasedOn('fpkg_chain_d', 'fpkg_chain_missing'), weapons.Get('fpkg_chain_c').PrintName, weapons.Get('fpkg_chain_d')
end)
registry_step('scripted_ents chain', function()
  scripted_ents.Register({ Type = 'anim', PrintName = 'A' }, 'fpkg_echain_a')
  scripted_ents.Register({ Base = 'fpkg_echain_a' }, 'fpkg_echain_b')
  scripted_ents.Register({ Base = 'fpkg_echain_b' }, 'fpkg_echain_c')
  scripted_ents.Register({ Base = 'fpkg_echain_missing', Type = 'anim' }, 'fpkg_echain_d')

  return scripted_ents.IsBasedOn('fpkg_echain_c', 'fpkg_echain_a'), scripted_ents.IsBasedOn('fpkg_echain_c', 'fpkg_echain_b'), scripted_ents.IsBasedOn('fpkg_echain_b', 'fpkg_echain_a'),
    scripted_ents.IsBasedOn('fpkg_echain_a', 'base_anim'), scripted_ents.IsBasedOn('fpkg_echain_d', 'fpkg_echain_missing'), scripted_ents.Get('fpkg_echain_c').PrintName, scripted_ents.Get('fpkg_echain_d'),
    scripted_ents.GetType('fpkg_echain_c'), scripted_ents.GetMember('fpkg_echain_c', 'PrintName')
end)
registry_step('baseclass after Register', function()
  return baseclass.Get('fpkg_echain_a'), baseclass.Get('fpkg_chain_a')
end)
registry_step('OnLoaded', function()
  weapons.OnLoaded()
  scripted_ents.OnLoaded()

  return baseclass.Get('fpkg_echain_a'), baseclass.Get('fpkg_chain_a'), baseclass.Get('fpkg_chain_a') == weapons.GetStored('fpkg_chain_a'),
    baseclass.Get('fpkg_echain_a') == scripted_ents.GetStored('fpkg_echain_a').t, weapons.GetStored('fpkg_chain_a').ThisClass, scripted_ents.GetStored('fpkg_echain_a').t.ThisClass,
    weapons.GetStored('fpkg_chain_a').Base
end)

local function reports_of(f, ...)
  local reported = {}

  local function keep(message)
    reported[#reported + 1] = normalized((tostring(message):gsub('\n.*', '')))
  end

  REPORTED = keep

  local results = pack(pcall(f, ...))

  REPORTED = nil

  if not results[1] then results[2] = normalized(results[2]) end

  return results, reported
end

local noop = named('noop', function() end)

for _, args in ipairs({
  { 'Add', nil, 'x', noop }, { 'Add', 'MisuseEvent', nil, noop }, { 'Add', 'MisuseEvent', 'x', nil }, { 'Add', 'MisuseEvent', 'x', 'f' },
  { 'Add', 'MisuseEvent', 5, noop }, { 'Add', 'MisuseEvent', true, noop }, { 'Add', 5, 'x', noop }, { 'Add', 'MisuseEvent', {}, noop },
  { 'Remove', nil, 'x' }, { 'Remove', 'MisuseEvent', nil }, { 'Remove', 5, 'x' }, { 'Remove', 'MisuseEvent', 5 }, { 'Call', nil }, { 'Run', nil },
  { 'Call', 'MisuseEvent', false }, { 'Call', 'MisuseEvent', 5 }
}) do
  call('misuse hook.'..args[1], function(name, ...) return reports_of(hook[name], ...) end, args[1], args[2], args[3], args[4])
end

for _, args in ipairs({ { nil, noop }, { 'fpkg_misuse', nil }, { 5, noop }, { 'fpkg_misuse_two', 'x' }, { {}, noop } }) do
  call('misuse concommand.Add', function(...) return reports_of(concommand.Add, ...) end, args[1], args[2])
end

call('misuse concommand.Remove', function() return reports_of(concommand.Remove, nil) end)
call('misuse hook table', function()
  local t = hook.GetTable()

  return t.MisuseEvent
end)

step('GetTable edit add', function()
  hook.Add('HookEdit', 'a', logged('edit a'))
  hook.GetTable().HookEdit.x = logged('edit x')

  return hook.Call('HookEdit', nil, 1)
end)
step('GetTable edit remove then Add', function()
  hook.GetTable().HookEdit.a = nil
  hook.Add('HookEdit', 'a', logged('edit a again'))

  return hook.Call('HookEdit', nil, 2)
end)
step('GetTable edit remove', function()
  hook.GetTable().HookEdit.x = nil

  return hook.Call('HookEdit', nil, 3)
end)
step('GetTable new event', function()
  hook.GetTable().HookEditNew = {}
  hook.Add('HookEditNew', 'n', logged('edit new'))

  return hook.Call('HookEditNew', nil, 4), hook.GetTable().HookEditNew
end)
step('GetTable replace function', function()
  hook.Add('HookEditReplace', 'r', logged('replace one'))
  hook.GetTable().HookEditReplace.r = logged('replace two')

  return hook.Call('HookEditReplace', nil, 5)
end)
step('removed and added again while called', function()
  hook.Add('HookReAdd', 'only', named('re-adder', function()
    log[#log + 1] = 're-adder'
    hook.Remove('HookReAdd', 'only')
    hook.Add('HookReAdd', 'only', logged('added again'))
  end))

  local first = pack(hook.Call('HookReAdd', { HookReAdd = logged('gm.HookReAdd', 'gm') }))
  local again = pack(hook.Call('HookReAdd', { HookReAdd = logged('gm.HookReAdd', 'gm') }))

  return first, again, hook.GetTable().HookReAdd
end)
step('false gamemode', function() return hook.Call('HookFallback', false, 1), hook.Call('NoSuchEvent', false) end)

for _, name in ipairs({ 'HookEdit', 'HookEditNew', 'HookEditReplace', 'HookReAdd', 'MisuseEvent' }) do
  for id in pairs(table.Copy(hook.GetTable()[name] or {})) do
    hook.Remove(name, id)
  end
end

call('table.Add self', function()
  local t = { 1, 2, 3 }
  local r = table.Add(t, t)

  return r == t, t
end)
call('table.Add hash', function()
  local r = table.Add({ 1 }, { x = 2, y = 3 })

  table.sort(r)

  return r
end)
call('table.Add mixed', function()
  local r = table.Add({ 'a' }, { 2, 3, k = 4 })

  return #r, r[1], r[2], r[3]
end)
call('table.Add nil target', function() return table.Add(nil, { 1, 2 }) end)
call('table.Add nil source', function() return table.Add({ 1 }, nil) end)

for _, desc in ipairs({ false, true }) do
  call('SortedPairsByMemberValue strings', function(d)
    local out = {}

    for k in SortedPairsByMemberValue({ a = { n = 'b' }, b = { n = 'A' }, c = { n = 'C' } }, 'n', d) do out[#out + 1] = k end

    return table.concat(out, ' ')
  end, desc)
  call('SortedPairsByMemberValue missing', function(d)
    local out = {}

    for k in SortedPairsByMemberValue({ a = { n = 2 }, b = { m = 1 }, c = { n = 1 } }, 'n', d) do out[#out + 1] = k end

    return table.concat(out, ' ')
  end, desc)
end

local utf8_pieces = {
  'a', '\128', '\191', '\192\128', '\193\191', '\194\128', '\224\128\128', '\224\159\191', '\224\160\128', '\237\160\128',
  '\239\191\191', '\240\128\128\128', '\240\143\191\191', '\244\143\191\191', '\244\144\128\128', '\247\191\191\191',
  '\248\136\128\128\128', '\252\132\128\128\128\128', '\254', '\255', '\226\130', '\226', 'é', '€', '😀'
}

for _ = 1, 400 do
  local parts = {}

  for i = 1, rand(5) - 1 do parts[i] = utf8_pieces[rand(#utf8_pieces)] end

  local s = table.concat(parts)

  call('utf8 overlong len', utf8.len, s)
  call('utf8 overlong force', utf8.force, s)
  call('utf8 overlong codepoint', utf8.codepoint, s, 1, -1)
  call('utf8 overlong codes', function(x)
    local out = {}
    local ok, err = pcall(function()
      for p, c in utf8.codes(x) do out[#out + 1] = p..':'..c end
    end)

    return table.concat(out, ' '), ok, ok or normalized(err)
  end, s)
  call('utf8 overlong offset', utf8.offset, s, rand(5) - 3, rand(#s + 1))
  call('utf8 overlong match', function(x)
    local out = {}

    for c in x:gmatch(utf8.charpattern) do out[#out + 1] = c end

    return out
  end, s)
end

call('utf8 codes stop', function()
  local out = {}
  local ok, err = pcall(function()
    for p, c in utf8.codes('a\128') do out[#out + 1] = p..':'..c end
  end)

  return table.concat(out, ' '), ok, normalized(err)
end)
call('utf8 offset continuation', utf8.offset, '\128a', -1, 2)
call('utf8 char nil', utf8.char, 65, nil, 66)
call('utf8 char nil end', utf8.char, 65, nil)

local function copy_report(original, copy)
  return {
    same = copy == original,
    meta = getmetatable(copy) == getmetatable(original),
    a_is_b = copy.a ~= nil and copy.a == copy.b,
    a_kept = copy.a ~= nil and copy.a == original.a,
    self_is_copy = copy.self ~= nil and copy.self == copy,
    deep_is_copy = copy.a ~= nil and type(copy.a) == 'table' and copy.a.up == copy,
    inner = copy.a and type(copy.a) == 'table' and copy.a.x
  }
end

call('table.Copy metatable field', function()
  local mt = { __metatable = 'locked', __index = { y = 1 } }
  local t = setmetatable({ x = 1 }, mt)
  local c = table.Copy(t)

  return c.x, c.y, debug.getmetatable(c) == mt
end)
call('table.Copy same leaf', function()
  local leaf = { x = 1 }
  local t = { a = leaf, b = leaf }

  return copy_report(t, table.Copy(t))
end)
call('table.Copy cycle', function()
  local t = { a = { x = 2 } }

  t.self = t
  t.a.up = t

  return copy_report(t, table.Copy(t))
end)
call('table.Copy same subtree', function()
  local subtree = { x = 3, deeper = { y = 4 } }
  local t = { a = subtree, b = subtree }
  local c = table.Copy(t)

  return copy_report(t, c), c.a.deeper == c.b.deeper
end)
call('table.Copy sibling cycle', function()
  local t = { a = { x = 5 } }

  t.b = { link = t.a }
  t.a.back = t.b

  local c = table.Copy(t)

  return c.a.back == c.b, c.b.link == c.a, c.a.back.link == c.a
end)

call('ToString self', function()
  local t = { 1 }

  t[2] = t

  return (table.ToString(t, 'n', false):gsub('table: 0x%x+', 'table'))
end)
call('ToString self nice', function()
  local t = { 'x' }

  t[2] = t

  return (table.ToString({ t }, nil, true):gsub('table: 0x%x+', 'table'))
end)
call('ToString color', function() return table.ToString({ Color(1, 2, 3, 4) }, 'c', false), table.ToString({ c = Color(255, 0, 0) }, nil, true) end)

local function tinted_class(base)
  return { Base = base, Tint = Color(1, 2, 3, 4), Nested = { Tint = Color(5, 6, 7, 8) } }
end

registry_step('Get keeps metatables', function()
  scripted_ents.Register(tinted_class(), 'fpkg_tinted_ent')
  weapons.Register(tinted_class(), 'fpkg_tinted_weapon')

  local ent, swep = scripted_ents.Get('fpkg_tinted_ent'), weapons.Get('fpkg_tinted_weapon')

  return IsColor(ent.Tint), IsColor(ent.Nested.Tint), IsColor(swep.Tint), IsColor(swep.Nested.Tint), ent.Tint ~= scripted_ents.GetStored('fpkg_tinted_ent').t.Tint
end)
registry_step('Get copies', function()
  local base = { Plain = { 1 }, Shared = { x = 1 }, Tint = Color(9, 9, 9), Only = { base = true } }
  local child = { Base = 'fpkg_copy_base', Plain = { 2 }, Shared = { y = 2 }, Tint = Color(1, 1, 1), Nested = { Tint = Color(2, 2, 2), Deep = { 3 } } }

  scripted_ents.Register(base, 'fpkg_copy_base')
  scripted_ents.Register(child, 'fpkg_copy_child')

  local copy = scripted_ents.Get('fpkg_copy_child')

  return copy.Tint == child.Tint, copy.Nested == child.Nested, copy.Nested.Tint == child.Nested.Tint, copy.Nested.Deep == child.Nested.Deep,
    copy.Plain == child.Plain, copy.Shared == child.Shared, copy.Only == base.Only, copy.BaseClass == base, IsColor(copy.BaseClass.Tint),
    copy.BaseClass.Tint == base.Tint, getmetatable(copy.Nested) == getmetatable(child.Nested), copy.Shared.BaseClass == base.Shared
end)
registry_step('Get copies metatables', function()
  local meta = { __index = { inherited = true } }
  local class = { Marked = setmetatable({ q = 1 }, meta), Inner = { Marked = setmetatable({ r = 2 }, meta) } }

  scripted_ents.Register(class, 'fpkg_meta_ent')

  local copy = scripted_ents.Get('fpkg_meta_ent')

  return copy.Marked == class.Marked, copy.Inner.Marked == class.Inner.Marked, getmetatable(copy.Marked) == meta, copy.Marked.inherited
end)
registry_step('Get copies colors with a base', function()
  scripted_ents.Register({ Tint = Color(9, 9, 9), Box = { Tint = Color(8, 8, 8) } }, 'fpkg_tint_base')
  scripted_ents.Register({ Base = 'fpkg_tint_base', Tint = Color(1, 1, 1), Box = { Tint = Color(2, 2, 2) } }, 'fpkg_tint_child')

  local copy = scripted_ents.Get('fpkg_tint_child')

  return copy.Tint, copy.Box.Tint, copy.Tint.BaseClass ~= nil, copy.Box.BaseClass ~= nil
end)
registry_step('Get copies weapons', function()
  local base = { Plain = { 1 }, Shared = { x = 1 }, Tint = Color(9, 9, 9) }
  local child = { Base = 'fpkg_wcopy_base', Plain = { 2 }, Shared = { y = 2 }, Tint = Color(1, 1, 1), Nested = { Deep = { 3 } } }

  weapons.Register(base, 'fpkg_wcopy_base')
  weapons.Register(child, 'fpkg_wcopy_child')

  local copy = weapons.Get('fpkg_wcopy_child')

  return copy.Tint == child.Tint, copy.Nested == child.Nested, copy.Nested.Deep == child.Nested.Deep, copy.Plain == child.Plain, copy.Shared == child.Shared,
    copy.BaseClass == base, copy.Shared.BaseClass == base.Shared, IsColor(copy.Tint)
end)
registry_step('baseclass Get sets Base globals', function()
  local saved_ent, saved_swep = ENT, SWEP

  ENT, SWEP = {}, {}

  baseclass.Get('fpkg_globals_base')

  local ent_base, swep_base = ENT.Base, SWEP.Base

  ENT, SWEP = nil, nil

  local without = pcall(baseclass.Get, 'fpkg_globals_base')

  ENT, SWEP = saved_ent, saved_swep

  return ent_base, swep_base, without
end)
registry_step('GetType of base classes', function()
  local out = {}

  for _, name in ipairs({ 'base_anim', 'base_point', 'base_brush', 'base_filter', 'base_ai', 'base_nextbot', 'base_entity', 'base_gmodentity', 'nope' }) do
    out[#out + 1] = name..'='..tostring(scripted_ents.GetType(name))
  end

  return out
end)
registry_step('weapon_base Base', function()
  weapons.Register({ PrintName = 'Weapon base' }, 'weapon_base')

  local copy = weapons.Get('weapon_base')

  return copy.Base, copy.BaseClass ~= nil, weapons.GetStored('weapon_base').Base, weapons.IsBasedOn('weapon_base', 'weapon_base')
end)
registry_step('ClassNameOverride', function()
  scripted_ents.Register({ ClassNameOverride = 'fpkg_override_ent', Type = 'anim' }, 'fpkg_original_ent')
  weapons.Register({ ClassNameOverride = 'fpkg_override_weapon' }, 'fpkg_original_weapon')

  return scripted_ents.GetStored('fpkg_override_ent') ~= nil, scripted_ents.GetStored('fpkg_original_ent') ~= nil, weapons.GetStored('fpkg_override_weapon') ~= nil,
    weapons.GetStored('fpkg_original_weapon') ~= nil, (scripted_ents.GetStored('fpkg_override_ent') or scripted_ents.GetStored('fpkg_original_ent')).t.ClassName,
    (weapons.GetStored('fpkg_override_weapon') or weapons.GetStored('fpkg_original_weapon')).ClassName
end)

for _, path in ipairs({ '.bashrc', 'a/.bashrc', 'a.b.c', '.', '..', 'a.', 'dir.d/file' }) do
  call('cases StripExtension', string.StripExtension, path)
  call('cases GetExtensionFromFilename', string.GetExtensionFromFilename, path)
end

for _, pattern in ipairs({ 'x*', '%s*', '.-', 'a?', '()', 'b*', '%d*', '' }) do
  for _, text in ipairs({ 'abc', '', 'a b', 'xx', '1a22' }) do
    call('cases Explode empty', string.Explode, pattern, text, true)
  end
end

for _, v in ipairs({ 0, 1, 59, 60, 61, -5 }) do call('cases NiceTime', string.NiceTime, v) end

call('cases NiceTime', string.NiceTime, nil)
call('cases NiceTime', string.NiceTime, '90')
call('cases ToTable', string.ToTable, 123)
call('cases ToTable', string.ToTable, 1.5)

for _, text in ipairs({ '1,2,3', '1 2 3', '1 2 3 4 5', '', 'a b c', '1.5 2.5 3.5 4.5', '-1 300 2 1', '1  2  3' }) do
  call('cases ToColor', function(x) return string.ToColor(x) end, text)
end

call('cases Interpolate', string.Interpolate, '{a}{b}{c}{d}', { a = false, b = true, c = 0, d = '' })
call('cases index', function(x) return x['2'], x['1.0'], x[2.0], x['x'], x[0], x[-1] end, 'abc')

for _, name in ipairs({ 'npc_antlionguardian', 'npc_antlion_grub', 'npc_advisor', 'npc_ministrider', 'npc_vortigaunt_slave', 'npc_combine_s_elite', 'npc_metropolice_hl1', 'NPC_ZOMBIE' }) do
  call('cases entity names extra', function(n) return IsEnemyEntityName(n), IsFriendEntityName(n) end, name)
end

call('cases type newproxy', function() return type(newproxy()), type(newproxy(true)), TypeID(newproxy()) end)
call('util.DateStamp', function()
  local saved = os.date

  os.date = function(layout, t)
    if layout == '*t' or layout == '!*t' then
      return { year = 2026, month = 3, day = 7, hour = 4, min = 5, sec = 6, wday = 7, yday = 66, isdst = false }
    end

    return 'layout:'..tostring(layout)
  end

  local ok, result = pcall(util.DateStamp)

  os.date = saved

  return ok, result
end)

call('PrintTable shared', console_of, function()
  local shared = { x = 1 }

  PrintTable({ a = shared, b = shared, c = { d = shared } })
end)
call('PrintTable deep cycle', console_of, function()
  local t = { a = { b = {} } }

  t.a.b.top = t
  t.a.b.mid = t.a

  PrintTable(t)
end)

for _, force in ipairs({ 'FORCE_COLOR', 'FORCE_VECTOR', 'FORCE_ANGLE' }) do
  for _, v in ipairs({ Color(1, 2, 3, 4), '1 2 3 4', '1 2 3', 5, Vector(1, 2, 3) }) do
    call('AccessorFunc '..force, function(f, value)
      local t = {}

      AccessorFunc(t, 'k', 'Thing', _G[f])

      local ok, err = pcall(t.SetThing, t, value)

      return ok, ok or normalized(err), t:GetThing(), IsColor(t.k)
    end, force, v)
  end
end

for _, args in ipairs({ { 2, 0, 1, 10, 1 }, { -1, 0, 1, 10, 1 }, { 0.5, 0, 1, 10, 1 }, { 1, 0, 1, 10, 1 }, { 3, 2, 0, 4, 0 } }) do
  call('cases CHSpline', math.CHSpline, unpack(args))
end

for _, args in ipairs({ { 1, 1, 0, 0 }, { 1, 2, 0, 0 }, { 0, 4, 0.5, 0 }, { 3, 3, 0, 0 } }) do
  call('cases calcBSplineN zero step', math.calcBSplineN, unpack(args))
end

for _, n in ipairs({ -1, -2, -5, -8, -1000, 0.5, 2.5, -0.5 }) do
  call('cases IntToBin', math.IntToBin, n)
end

cvars_step('exact callback names', function()
  local cv = CreateConVar('fpkg_compare_case', '1')

  cvars.AddChangeCallback('FPKG_COMPARE_CASE', changed('upper'), 'upper')
  cvars.AddChangeCallback('fpkg_compare_case', changed('lower'), 'lower')
  cv:SetString('2')

  return pack(cvars.GetConVarCallbacks('FPKG_COMPARE_CASE') ~= nil, cvars.GetConVarCallbacks('fpkg_compare_case') ~= nil)
end)
cvars_step('removal by exact name', function()
  cvars.RemoveChangeCallback('FPKG_COMPARE_case', 'lower')
  CreateConVar('fpkg_compare_case', '1'):SetString('3')

  return true
end)

local libraries = {
  { '^utf8', 'utf8' }, { '^ease%.', 'math.ease' }, { '^hook ', 'hook' }, { '^concommand ', 'concommand' },
  { '^cvars ', 'cvars' }, { '^registry ', 'baseclass, weapons, scripted_ents' }, { '^Color', 'colors' },
  { '^HSV', 'colors' }, { '^HSL', 'colors' }, { '^HWB', 'colors' }, { '^HexToColor', 'colors' }, { '^IsColor', 'colors' },
  { '^color ', 'colors' }, { '^SortedPairs', 'table' }, { '^index', 'string' }, { '^NaN', 'math' }
}

local function library_of(name)
  name = name:gsub('^cases ', '')

  for _, rule in ipairs(libraries) do
    if name:find(rule[1]) then return rule[2] end
  end

  local base = name:match('^[%w_]+')

  if rawget(string, base) then return 'string' end
  if rawget(table, base) then return 'table' end
  if rawget(math, base) then return 'math' end

  return 'util and globals'
end

local counts = {}

for _, line in ipairs(lines) do
  local library = library_of(line:match('^[^(]*'))

  counts[library] = (counts[library] or 0) + 1
end

local summary = {}

for library, count in SortedPairs(counts) do
  summary[#summary + 1] = library..': '..count
end

return table.concat(lines, '\n'), table.concat(summary, ', ')
