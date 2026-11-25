-- fpkg's GLua runtime: the Lua half of the `sql` library, on the engine's `sql.Query`.
-- `sql.SQLStr` and the global `SQLStr` (a value as an SQLite string literal, cut at its
-- first NUL, unquoted when asked); `sql.LastError`; `sql.QueryRow` and `sql.QueryValue`
-- (`false` when the query failed, `nil` when it found nothing); `sql.TableExists` and
-- `sql.IndexExists`; `sql.Begin` and `sql.Commit`.

local tostring, find, sub, gsub = tostring, string.find, string.sub, string.gsub

function sql.SQLStr(value, no_quotes)
  local text = tostring(value)
  local nul = find(text, '\0', 1, true)

  if nul then
    text = sub(text, 1, nul - 1)
  end

  text = gsub(text, "'", "''")

  if no_quotes then
    return text
  end

  return "'"..text.."'"
end

SQLStr = sql.SQLStr

function sql.LastError()
  return sql.m_strError
end

function sql.QueryRow(text, row)
  local rows = sql.Query(text)

  if not rows then
    return rows
  end

  return rows[row or 1]
end

function sql.QueryValue(text)
  local row = sql.QueryRow(text)

  if not row then
    return row
  end

  local _, value = next(row)

  return value
end

local function schema_has(kind, name)
  local row = sql.QueryRow('SELECT name FROM sqlite_master WHERE type = '..sql.SQLStr(kind)..' AND name = '..sql.SQLStr(name))

  return type(row) == 'table'
end

function sql.TableExists(name)
  return schema_has('table', name)
end

function sql.IndexExists(name)
  return schema_has('index', name)
end

function sql.Begin()
  sql.Query('BEGIN;')
end

function sql.Commit()
  sql.Query('COMMIT;')
end
