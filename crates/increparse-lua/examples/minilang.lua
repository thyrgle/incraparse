-- MiniLang, defined entirely in Lua.
--
-- Language shape:
--
--   def add(a, b) { return a + b; }
--   def bad(x) { return; }
--
-- Three passes:
--   round 0: file -> one region per `def name(params) { ... }`
--   round 1: function -> one region per `return ...;`
--   round 2: return -> "done" if the expression is non-empty
--
-- Use with:
--   increparse-lua-server /path/to/this/file.lua

local function skip_ws(s, i)
  while i <= #s and s:sub(i, i):match("%s") do
    i = i + 1
  end
  return i
end

-- Round 0: the file.
local function functions_pass(source, span, ctx)
  if ctx.File == nil then
    return "failed"
  end

  local children = {}
  local i = span.start + 1 -- Lua strings are 1-based; spans are 0-based
  local limit = span["end"]

  while i <= limit do
    i = skip_ws(source, i)
    if i > limit then
      break
    end

    local name = source:match("^def%s+([%w_]+)", i)
    if not name then
      i = i + 1 -- one-byte resync: malformed lines cost nothing
    else
      local open_paren = source:find("(", i, true)
      if not open_paren then
        break
      end
      -- Parameter lists live on one line: a missing `)` costs the line,
      -- never the definitions after it.
      local nl = source:find("\n", open_paren, true)
      local line_end = nl and (nl - 1) or limit
      local close_paren = source:find(")", open_paren + 1, true)
      if close_paren == nil or close_paren > line_end then
        i = line_end + 1 -- malformed header line: skip it, keep scanning
      else
        local params = {}
        for p in source:sub(open_paren + 1, close_paren - 1):gmatch("[%w_]+") do
          params[#params + 1] = p
        end

        local open_brace = source:find("{", close_paren + 1, true)
        if not open_brace then
          i = close_paren + 1
        else
          local depth, close_brace = 1, nil
          for j = open_brace + 1, limit do
            local ch = source:sub(j, j)
            if ch == "{" then
              depth = depth + 1
            elseif ch == "}" then
              depth = depth - 1
              if depth == 0 then
                close_brace = j
                break
              end
            end
          end

          if not close_brace then
            break
          end

          children[#children + 1] = {
            start = i - 1,
            ["end"] = close_brace,
            ctx = { Function = { name = name, params = params } },
          }
          i = close_brace + 1
        end
      end
    end
  end

  return { expand = children }
end

-- Round 1: function bodies.
local function body_pass(source, span, ctx)
  if ctx.Function == nil then
    return "failed"
  end

  local children = {}
  local i = span.start + 1
  local limit = span["end"]

  while i <= limit do
    local s0, e0 = source:find("return", i, true)
    if not s0 or e0 > limit then
      break
    end
    local semi = source:find(";", e0 + 1, true)
    if not semi or semi > limit then
      break
    end
    children[#children + 1] = {
      start = s0 - 1,
      ["end"] = semi,
      ctx = { Return = { ["function"] = ctx.Function.name } },
    }
    i = semi + 1
  end

  return { expand = children }
end

-- Round 2: the checker.
local function return_pass(source, span, ctx)
  if ctx.Return == nil then
    return "failed"
  end
  local text = source:sub(span.start + 1, span["end"])
  local expr = text:match("^return%s*(.-)%s*;$")
  if expr == nil or expr == "" then
    return "failed"
  end
  return "done"
end

return {
  name = "minilang",
  root_ctx = { File = true },
  passes = { functions_pass, body_pass, return_pass },

  diagnostic = function(source, node)
    if node.ctx and node.ctx.Return then
      return {
        message = "empty return in `" .. node.ctx.Return["function"] .. "`",
        severity = 1,
      }
    end
    return nil
  end,

  symbols = function(nodes)
    local out = {}
    for _, n in ipairs(nodes) do
      local c = n.ctx
      if c and c.Function then
        out[#out + 1] = {
          name = c.Function.name,
          detail = "(" .. table.concat(c.Function.params, ", ") .. ")",
          start = n.start,
          ["end"] = n["end"],
        }
      end
    end
    return out
  end,
}
