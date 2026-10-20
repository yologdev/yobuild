// The page's logic that needs no DOM, shared by app.js and its tests
// (`node --test web/`).

// Markdown for answers (design §5, "Answers as Markdown on the page"): a
// subset of CommonMark with GFM tables, read into data the page builds as
// DOM nodes — never HTML from the model. Images, raw HTML, reference links
// and footnotes stay text; only https links become links.

/** How deep blocks and inline marks may nest before the rest is text. */
export const MAX_DEPTH = 8

/**
 * The blocks of `text`: `heading{level, inline}`, `paragraph{inline}`,
 * `list{ordered, start, items: blocks[]}`, `code{lang, text}`,
 * `quote{blocks}`, `rule`, `table{align, header, rows}` (`align` per column:
 * 'left', 'center', 'right' or null; cells are inline parts).
 */
export function markdownBlocks(text) {
  return blocks(String(text ?? '').replace(/\r\n?/g, '\n').split('\n'), 0)
}

const FENCE = /^ {0,3}(`{3,}|~{3,})\s*([^\s`]*)/
const HEADING = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$/
const RULE = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/
const QUOTE = /^ {0,3}> ?/
const ITEM = /^( {0,3})([-*+]|\d{1,9}[.)])([ \t]+|$)(.*)$/
const SEPARATOR = /^ {0,3}\|?[ \t]*:?-+:?[ \t]*(\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$/

const blank = (line) => /^[ \t]*$/.test(line)
const indentOf = (line) => line.match(/^ */)[0].length

/** Whether `lines[i]` starts a table: a row, then a separator of as many cells. */
function tableAt(lines, i) {
  if (!lines[i].includes('|') || i + 1 >= lines.length || !SEPARATOR.test(lines[i + 1])) return false
  return cells(lines[i]).length === cells(lines[i + 1]).length
}

/** Whether `line` starts a block other than a paragraph's next line. */
function interrupts(lines, i) {
  const line = lines[i]
  return FENCE.test(line) || HEADING.test(line) || RULE.test(line) || QUOTE.test(line) ||
    (ITEM.test(line) && !blank(line.replace(ITEM, '$4'))) || tableAt(lines, i)
}

function blocks(lines, depth) {
  if (depth > MAX_DEPTH) {
    const text = lines.join('\n').trim()
    return text ? [{ kind: 'paragraph', inline: [{ kind: 'text', text }] }] : []
  }
  const out = []
  let i = 0
  while (i < lines.length) {
    const line = lines[i]
    if (blank(line)) {
      i++
      continue
    }
    const fence = line.match(FENCE)
    if (fence) {
      const [, mark, lang] = fence
      const close = new RegExp(`^ {0,3}${mark[0] === '`' ? '`' : '~'}{${mark.length},}[ \\t]*$`)
      const body = []
      i++
      // An unclosed fence runs to the end (a streaming reply's open block).
      while (i < lines.length && !close.test(lines[i])) body.push(lines[i++])
      i++
      out.push({ kind: 'code', lang, text: body.join('\n') })
      continue
    }
    const heading = line.match(HEADING)
    if (heading) {
      out.push({ kind: 'heading', level: heading[1].length, inline: inline(heading[2] ?? '', depth) })
      i++
      continue
    }
    if (RULE.test(line)) {
      out.push({ kind: 'rule' })
      i++
      continue
    }
    if (QUOTE.test(line)) {
      const body = []
      while (i < lines.length && QUOTE.test(lines[i])) body.push(lines[i++].replace(QUOTE, ''))
      out.push({ kind: 'quote', blocks: blocks(body, depth + 1) })
      continue
    }
    if (tableAt(lines, i)) {
      const align = cells(lines[i + 1]).map((cell) => {
        const left = cell.startsWith(':')
        const right = cell.endsWith(':')
        return left && right ? 'center' : right ? 'right' : left ? 'left' : null
      })
      const row = (l) => {
        const got = cells(l).slice(0, align.length)
        while (got.length < align.length) got.push('')
        return got.map((cell) => inline(cell, depth))
      }
      const header = row(lines[i])
      const rows = []
      i += 2
      while (i < lines.length && !blank(lines[i]) && lines[i].includes('|')) rows.push(row(lines[i++]))
      out.push({ kind: 'table', align, header, rows })
      continue
    }
    const item = line.match(ITEM)
    if (item) {
      const [list, next] = listAt(lines, i, depth)
      out.push(list)
      i = next
      continue
    }
    const body = [line.trim()]
    i++
    while (i < lines.length && !blank(lines[i]) && !interrupts(lines, i)) body.push(lines[i++].trim())
    out.push({ kind: 'paragraph', inline: inline(body.join('\n'), depth) })
  }
  return out
}

/** The list starting at `lines[i]`, and the index after it. */
function listAt(lines, i, depth) {
  const first = lines[i].match(ITEM)
  const ordered = /\d/.test(first[2])
  const kind = (marker) => (/\d/.test(marker) ? marker.slice(-1) : marker)
  const base = first[1].length
  const items = []
  while (i < lines.length) {
    const m = lines[i].match(ITEM)
    if (!m || m[1].length < base || m[1].length > base + 3 || kind(m[2]) !== kind(first[2])) break
    // Content starts after the marker and its spaces (a marker alone: 2 on).
    const content = m[1].length + m[2].length + (m[4] ? Math.min(m[3].length, 4) : 1)
    const body = [m[4]]
    i++
    while (i < lines.length) {
      const line = lines[i]
      if (blank(line)) {
        // A blank line stays in the item when what follows is indented to it.
        let j = i
        while (j < lines.length && blank(lines[j])) j++
        if (j < lines.length && indentOf(lines[j]) >= content) {
          while (i < j) body.push(lines[i++].slice(content))
          continue
        }
        break
      }
      if (indentOf(line) >= content) {
        body.push(line.slice(content))
        i++
        continue
      }
      // A line of its own paragraph, not indented (lazy continuation).
      if (!interrupts(lines, i) && !blank(body[body.length - 1] ?? '')) {
        body.push(line.trim())
        i++
        continue
      }
      break
    }
    items.push(blocks(body, depth + 1))
    // Blank lines between items keep the list.
    let j = i
    while (j < lines.length && blank(lines[j])) j++
    if (j > i && j < lines.length && ITEM.test(lines[j]) && indentOf(lines[j]) >= base && indentOf(lines[j]) <= base + 3) i = j
  }
  const start = ordered ? Number.parseInt(first[2], 10) : 1
  return [{ kind: 'list', ordered, start, items }, i]
}

/** A table row's cells: outer pipes dropped, split at unescaped pipes. */
function cells(line) {
  let row = line.trim()
  if (row.startsWith('|')) row = row.slice(1)
  if (row.endsWith('|') && !row.endsWith('\\|')) row = row.slice(0, -1)
  const out = []
  let cell = ''
  for (let k = 0; k < row.length; k++) {
    if (row[k] === '\\' && row[k + 1] === '|') {
      cell += '|'
      k++
    } else if (row[k] === '|') {
      out.push(cell.trim())
      cell = ''
    } else {
      cell += row[k]
    }
  }
  out.push(cell.trim())
  return out
}

/**
 * The inline parts of `text`: `text{text}`, `br`, `code{text}`,
 * `link{text, url}`, and `strong`/`em`/`del{parts}`. Only https links.
 */
export function markdownParts(text) {
  return inline(String(text ?? ''), 0)
}

const PUNCTUATION = /[!-/:-@[-`{-~]/
const WORD = /[\p{L}\p{N}]/u

function inline(text, depth) {
  const parts = []
  let plain = ''
  const flush = () => {
    if (plain) parts.push({ kind: 'text', text: plain })
    plain = ''
  }
  if (depth > MAX_DEPTH) return text ? [{ kind: 'text', text }] : []
  const closers = closerIndex(text)
  let i = 0
  while (i < text.length) {
    const c = text[i]
    if (c === '\\' && i + 1 < text.length && PUNCTUATION.test(text[i + 1])) {
      plain += text[i + 1]
      i += 2
      continue
    }
    if (c === '\n') {
      flush()
      parts.push({ kind: 'br' })
      i++
      continue
    }
    if (c === '`') {
      const run = text.slice(i).match(/^`+/)[0]
      const end = closers.backticks(run.length, i + run.length)
      if (end >= 0) {
        let code = text.slice(i + run.length, end).replace(/\n/g, ' ')
        if (/^ .*[^ ].* $/.test(code)) code = code.slice(1, -1)
        flush()
        parts.push({ kind: 'code', text: code })
        i = end + run.length
        continue
      }
      plain += run
      i += run.length
      continue
    }
    if (c === '[') {
      const close = closers.bracket(i)
      const url = close >= 0 ? text.slice(close + 1).match(/^\(([^\s()]+)\)/) : null
      // An image (`![…](…)`) stays text, whole: its URL is never loaded,
      // nor offered.
      if (url && text[i - 1] === '!' && text[i - 2] !== '\\') {
        const whole = text.slice(i, close + 1 + url[0].length)
        plain += whole
        i += whole.length
        continue
      }
      if (url && url[1].startsWith('https://')) {
        flush()
        parts.push({ kind: 'link', text: text.slice(i + 1, close), url: url[1] })
        i = close + 1 + url[0].length
        continue
      }
    }
    if (c === 'h' && (i === 0 || !WORD.test(text[i - 1]))) {
      const auto = text.slice(i).match(/^https:\/\/[^\s<>]+/)
      if (auto) {
        // A stop, or a closing parenthesis the link did not open, is the
        // sentence's: cut until neither ends it.
        let url = auto[0]
        for (let cut = true; cut; ) {
          const before = url
          url = url.replace(/[.,;:!?'"]+$/, '')
          if (url.endsWith(')') && (url.match(/\(/g) ?? []).length < (url.match(/\)/g) ?? []).length) url = url.slice(0, -1)
          cut = url !== before
        }
        flush()
        parts.push({ kind: 'link', text: url, url })
        i += url.length
        continue
      }
    }
    if (c === '*' || c === '_' || c === '~') {
      const double = text[i + 1] === c
      if (c === '~' && !double) {
        plain += c
        i++
        continue
      }
      const mark = double ? c + c : c
      const after = text[i + mark.length]
      const before = text[i - 1]
      const opens = after !== undefined && !/\s/.test(after) && !(c === '_' && before !== undefined && WORD.test(before))
      let end = opens ? closers.mark(mark, i + mark.length + 1) : -1
      // A double closer in a longer run closes at the run's end: the inner
      // single mark (`**b *c***`) closes first.
      if (end >= 0 && double) while (text[end + 2] === c) end++
      if (end >= 0) {
        flush()
        const kind = c === '~' ? 'del' : double ? 'strong' : 'em'
        parts.push({ kind, parts: inline(text.slice(i + mark.length, end), depth + 1) })
        i = end + mark.length
        continue
      }
      plain += mark
      i += mark.length
      continue
    }
    plain += c
    i++
  }
  flush()
  return parts
}

/**
 * Where each closing mark of `text` is, found once: a lookup per opener
 * costs a binary search, not a scan, so no text makes reading quadratic.
 */
function closerIndex(text) {
  const marks = new Map()
  const runs = new Map()
  const add = (map, key, at) => {
    if (!map.has(key)) map.set(key, [])
    map.get(key).push(at)
  }
  for (let k = 0; k < text.length; k++) {
    const c = text[k]
    if (c === '\\') {
      k++
      continue
    }
    if (c === '`') {
      let n = 1
      while (text[k + n] === '`') n++
      add(runs, n, k)
      k += n - 1
      continue
    }
    if (c === '*' || c === '_' || c === '~') {
      const before = text[k - 1]
      // A closer follows a non-space; `_` is not followed by a word.
      for (const mark of [c + c, c]) {
        if (!text.startsWith(mark, k) || (mark === '~')) continue
        const after = text[k + mark.length]
        if (before === undefined || /\s/.test(before)) continue
        if (c === '_' && after !== undefined && WORD.test(after)) continue
        add(marks, mark, k)
      }
    }
  }
  const brackets = []
  for (let k = 0; k < text.length; k++) if (text[k] === ']' && text[k - 1] !== '\\') brackets.push(k)
  const firstFrom = (list, from) => {
    if (!list) return -1
    let lo = 0
    let hi = list.length
    while (lo < hi) {
      const mid = (lo + hi) >> 1
      if (list[mid] < from) lo = mid + 1
      else hi = mid
    }
    return lo < list.length ? list[lo] : -1
  }
  return {
    backticks: (n, from) => firstFrom(runs.get(n), from),
    mark: (mark, from) => firstFrom(marks.get(mark), from),
    bracket: (from) => firstFrom(brackets, from),
  }
}

// Questions wait in order: one shown at a time, oldest first. A question
// may arrive twice (in `hello` and as its own `uiRequest`): kept once.
export class Questions {
  constructor() {
    this.waiting = []
  }

  // Queue a question; returns whether it was new.
  add(id, request) {
    if (this.waiting.some((q) => q.id === id)) return false
    this.waiting.push({ id, request })
    return true
  }

  // Drop a question (answered here or elsewhere, withdrawn, timed out);
  // returns whether it was the one shown.
  remove(id) {
    const index = this.waiting.findIndex((q) => q.id === id)
    if (index < 0) return false
    this.waiting.splice(index, 1)
    return index === 0
  }

  // The question to show: the oldest.
  current() {
    return this.waiting[0]
  }

  clear() {
    this.waiting = []
  }
}

// A value as JSON text, for a line on screen. A big integer (beyond
// ±(2^53−1)) reaches a rutis 0.11 runtime as a BigInt, which JSON.stringify
// refuses: written as a string of its digits.
export function jsonText(value) {
  return JSON.stringify(value, (_key, v) => (typeof v === 'bigint' ? v.toString() : v))
}

// The line for a finished run.
export function runEndLine({ run, outcome, error, totalCostUsd }) {
  const cost = totalCostUsd != null ? ` · $${Number(totalCostUsd).toFixed(4)}` : ''
  switch (outcome) {
    case 'completed':
      return `run ${run} done${cost}`
    case 'aborted':
      return `run ${run} stopped${cost}`
    case 'rejected':
      return `run ${run} refused: ${error ?? 'input rejected'}`
    case 'stopped':
      return `run ${run} stopped early: ${error ?? 'a limit'}${cost}`
    default:
      return `run ${run} failed: ${error ?? 'unknown error'}${cost}`
  }
}

// Tool cards. A tool result may carry `details.view = {call?, result?}`: how
// the tool wants its call shown, in a small card vocabulary (the DeepSeek
// Harness one): generic, terminal, diff, search, read, web. Anything else is
// ignored, and the page falls back to the plain tool line.
const CARDS = new Set(['generic', 'terminal', 'diff', 'search', 'read', 'web'])

export function toolView(details) {
  const view = details?.view
  if (!view || typeof view !== 'object') return undefined
  const card = (c) => (c && typeof c === 'object' && CARDS.has(c.card) ? c : undefined)
  const call = card(view.call)
  const result = card(view.result)
  return call || result ? { call, result } : undefined
}

// The title to show for the call: the result's, else the call's.
export function viewTitle(view) {
  const title = view?.result?.title ?? view?.call?.title
  return typeof title === 'string' && title ? title : undefined
}

// A line diff (longest common subsequence): rows of `{op, text}`, op ' ', '-'
// or '+'. `oldText` null is a new file. When the old and new line counts
// multiplied exceed `max`², it does not align: every old line removed, every
// new one added.
export function diffRows(oldText, newText, max = 400) {
  const a = oldText == null ? [] : String(oldText).split('\n')
  const b = String(newText ?? '').split('\n')
  if (a.length * b.length > max * max) {
    return [...a.map((text) => ({ op: '-', text })), ...b.map((text) => ({ op: '+', text }))]
  }
  const lcs = Array.from({ length: a.length + 1 }, () => new Uint32Array(b.length + 1))
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      lcs[i][j] = a[i] === b[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1])
    }
  }
  const rows = []
  let i = 0
  let j = 0
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      rows.push({ op: ' ', text: a[i] })
      i++
      j++
    } else if (lcs[i + 1][j] >= lcs[i][j + 1]) rows.push({ op: '-', text: a[i++] })
    else rows.push({ op: '+', text: b[j++] })
  }
  while (i < a.length) rows.push({ op: '-', text: a[i++] })
  while (j < b.length) rows.push({ op: '+', text: b[j++] })
  return rows
}

// A link target the page may use: http(s) only.
export function safeUrl(url) {
  try {
    const parsed = new URL(String(url))
    return parsed.protocol === 'https:' || parsed.protocol === 'http:' ? parsed.href : undefined
  } catch {
    return undefined
  }
}

// A typed answer (the terminal UI) to a question: `{ value }`, or `{ error }`
// when it does not fit, so the question is asked again rather than answered
// with a guess. confirm: y/yes or n/no. select: a number (blank: no choice);
// with `multiple`, numbers split by commas or spaces, each at most once
// (blank: none). input: the text.
export function parseAnswer(request, text) {
  const trimmed = String(text ?? '').trim()
  if (request.kind === 'confirm') {
    if (/^(y|yes)$/i.test(trimmed)) return { value: true }
    if (/^(n|no)$/i.test(trimmed)) return { value: false }
    return { error: 'answer y or n' }
  }
  if (request.kind === 'select') {
    const options = request.options ?? []
    const pick = (token) => {
      const n = Number(token)
      return /^\d+$/.test(token) && n >= 1 && n <= options.length ? options[n - 1] : undefined
    }
    if (request.multiple) {
      const tokens = trimmed.split(/[\s,]+/).filter(Boolean)
      const bad = tokens.find((token) => pick(token) === undefined)
      if (bad !== undefined) return { error: `"${bad}" is not one of 1–${options.length}` }
      return { value: [...new Set(tokens.map(pick))] }
    }
    if (!trimmed) return { value: null }
    const choice = pick(trimmed)
    return choice === undefined ? { error: `answer with a number from 1 to ${options.length}` } : { value: choice }
  }
  return { value: String(text ?? '') }
}
