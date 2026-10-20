// The page's logic that needs no DOM of its own, shared by app.js and its
// tests (`node --test frontend/web/lib.test.js`).

// Markdown for answers (design §5, "Answers as Markdown on the page"): a
// subset of CommonMark with GFM tables, read into data, then drawn as nodes
// made one by one (`drawMarkdown`) — never HTML from the model. Images, raw
// HTML, reference links and footnotes stay text; only https links become
// links. Every step is linear in the text (no backtracking regex, closers
// found through indexes), as the page reads a streaming answer again with
// each delta.

/** How deep blocks, and the inline marks within them, may nest together
 * before the rest is text. */
export const MAX_DEPTH = 8

/**
 * The blocks of `text`: `heading{level, inline}`, `paragraph{inline}`,
 * `list{ordered, start, items: blocks[]}`, `code{lang, text}`,
 * `quote{blocks}`, `rule`, `table{align, header, rows}` (`align` per column:
 * 'left', 'center', 'right' or null; cells are inline parts).
 */
export function markdownBlocks(text) {
  const lines = String(text ?? '')
    .replace(/\r\n?/g, '\n')
    .split('\n')
    // Leading tabs count as four spaces (CommonMark's tab stop).
    .map((line) => (line.startsWith('\t') || line.startsWith(' ') ? line.replace(/^[ \t]+/, (ws) => ws.replace(/\t/g, '    ')) : line))
  return blocks(lines, 0)
}

const blank = (line) => line.trim() === ''
const indentOf = (line) => {
  let n = 0
  while (line[n] === ' ') n++
  return n
}

/** A fence's opening line: its mark and language; a backtick fence's info has no backtick. */
function fenceOf(line) {
  const m = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line)
  if (!m) return null
  const info = m[2].trim()
  if (m[1][0] === '`' && info.includes('`')) return null
  return { mark: m[1], lang: info.split(/[ \t]/)[0] }
}

/** A heading's level and text: `#` to `######`, then a space or the end; a closing run of `#` dropped. */
function headingOf(line) {
  let i = 0
  while (i < 3 && line[i] === ' ') i++
  let n = 0
  while (line[i + n] === '#') n++
  if (n < 1 || n > 6) return null
  const rest = line.slice(i + n)
  if (rest && rest[0] !== ' ' && rest[0] !== '\t') return null
  let body = rest.trim()
  let end = body.length
  while (end > 0 && body[end - 1] === '#') end--
  if (end === 0) body = ''
  else if (end < body.length && (body[end - 1] === ' ' || body[end - 1] === '\t')) body = body.slice(0, end).trimEnd()
  return { level: n, text: body }
}

/** A rule: three or more of one of `-*_`, spaces between. */
function ruleAt(line) {
  if (indentOf(line) > 3) return false
  const marks = line.replace(/[ \t]/g, '')
  return marks.length >= 3 && /^(?:-+|\*+|_+)$/.test(marks)
}

const QUOTE = /^ {0,3}> ?/
const ITEM = /^( {0,3})([-*+]|\d{1,9}[.)])([ \t]+|$)(.*)$/

/** A table's separator row: its cells, or null. */
function separatorOf(line) {
  if (!line.includes('-') || !/^[ \t|:-]+$/.test(line)) return null
  const row = cells(line)
  return row.every((cell) => /^:?-+:?$/.test(cell)) ? row : null
}

/** Whether `lines[i]` starts a table: a row, then a separator of as many cells. */
function tableAt(lines, i) {
  if (!lines[i].includes('|') || i + 1 >= lines.length) return false
  const separator = separatorOf(lines[i + 1])
  return separator !== null && cells(lines[i]).length === separator.length
}

/**
 * Whether `lines[i]` ends the paragraph before it: a block's start — a list
 * only when it has text, and a numbered one only from 1 (CommonMark: `2024.`
 * on a paragraph's next line is the paragraph's).
 */
function interrupts(lines, i) {
  const line = lines[i]
  const item = line.match(ITEM)
  const list = item && !blank(item[4]) && (!/\d/.test(item[2]) || Number.parseInt(item[2], 10) === 1)
  return Boolean(fenceOf(line) || headingOf(line) || ruleAt(line) || QUOTE.test(line) || list || tableAt(lines, i))
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
    const fence = fenceOf(line)
    if (fence) {
      const close = (l) => {
        const m = /^ {0,3}(`{3,}|~{3,})[ \t]*$/.exec(l)
        return m !== null && m[1][0] === fence.mark[0] && m[1].length >= fence.mark.length
      }
      const body = []
      i++
      // An unclosed fence runs to the end (a streaming reply's open block).
      while (i < lines.length && !close(lines[i])) body.push(lines[i++])
      i++
      out.push({ kind: 'code', lang: fence.lang, text: body.join('\n') })
      continue
    }
    const heading = headingOf(line)
    if (heading) {
      out.push({ kind: 'heading', level: heading.level, inline: inline(heading.text, depth) })
      i++
      continue
    }
    if (ruleAt(line)) {
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
      const align = separatorOf(lines[i + 1]).map((cell) => {
        const left = cell.startsWith(':')
        const right = cell.endsWith(':')
        return left && right ? 'center' : right ? 'right' : left ? 'left' : null
      })
      // Short rows padded; a long row's extra cells kept, in its last.
      const row = (l) => {
        const got = cells(l)
        if (got.length > align.length) got.splice(align.length - 1, got.length, got.slice(align.length - 1).join(' | '))
        while (got.length < align.length) got.push('')
        return got.map((cell) => inline(cell, depth))
      }
      // A table whose rows start with `|` ends at a line that does not.
      const piped = lines[i].trim().startsWith('|')
      const header = row(lines[i])
      const rows = []
      i += 2
      while (i < lines.length && !blank(lines[i]) && lines[i].includes('|') && (!piped || lines[i].trim().startsWith('|'))) {
        rows.push(row(lines[i++]))
      }
      out.push({ kind: 'table', align, header, rows })
      continue
    }
    if (ITEM.test(line)) {
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
    // Content starts after the marker and up to 4 spaces (a marker alone:
    // one column past it).
    const content = m[1].length + m[2].length + (m[4] ? Math.min(m[3].length, 4) : 1)
    const body = [m[4]]
    // A fence open in the item: its lines are the item's, never lazy.
    let fence = fenceOf(m[4])
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
        const own = line.slice(content)
        if (fence) {
          const close = /^ {0,3}(`{3,}|~{3,})[ \t]*$/.exec(own)
          if (close && close[1][0] === fence.mark[0] && close[1].length >= fence.mark.length) fence = null
        } else {
          fence = fenceOf(own)
        }
        body.push(own)
        i++
        continue
      }
      // A line of its own paragraph, not indented (lazy continuation) —
      // never another item, of this list or another.
      const item = line.match(ITEM)
      if (!fence && !interrupts(lines, i) && !(item && !blank(item[4])) && !blank(body[body.length - 1] ?? '')) {
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

/** A table row's cells: outer pipes dropped, split at pipes outside code spans and not escaped. */
function cells(line) {
  let row = line.trim()
  if (row.startsWith('|')) row = row.slice(1)
  if (row.endsWith('|') && !row.endsWith('\\|')) row = row.slice(0, -1)
  const spans = codeSpans(row)
  const out = []
  let cell = ''
  for (let k = 0; k < row.length; k++) {
    const span = spans.get(k)
    if (span) {
      cell += row.slice(k, span.end).replace(/\\\|/g, '|')
      k = span.end - 1
    } else if (row[k] === '\\' && row[k + 1] === '|') {
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
 * `link{parts, url}` (https only), and `strong`/`em`/`del{parts}`.
 */
export function markdownParts(text) {
  return inline(String(text ?? ''), 0)
}

const PUNCTUATION = /[!-/:-@[-`{-~]/
const PUNCT = /[\p{P}\p{S}]/u
const SPACE = /\s/
const WORD = /[\p{L}\p{N}]/u

/** An https URL the page may link to, or null. */
function linkUrl(url) {
  return url.startsWith('https://') && safeUrl(url) ? url : null
}

/**
 * The code spans of `text`, by where they start: `{end, text}`. A run of N
 * backticks closes at the next run of exactly N (backslashes are literal
 * inside); runs are indexed by length, so finding each costs a search.
 */
function codeSpans(text) {
  const runs = new Map()
  for (let k = 0; k < text.length; k++) {
    if (text[k] !== '`') continue
    let n = 1
    while (text[k + n] === '`') n++
    if (!runs.has(n)) runs.set(n, [])
    runs.get(n).push(k)
    k += n - 1
  }
  const spans = new Map()
  let i = 0
  while (i < text.length) {
    if (text[i] === '\\' && i + 1 < text.length && PUNCTUATION.test(text[i + 1])) {
      i += 2
      continue
    }
    if (text[i] !== '`') {
      i++
      continue
    }
    let n = 1
    while (text[i + n] === '`') n++
    const close = firstFrom(runs.get(n), i + n)
    if (close < 0) {
      i += n
      continue
    }
    let code = text.slice(i + n, close).replace(/\n/g, ' ')
    if (code.length > 2 && code[0] === ' ' && code[code.length - 1] === ' ' && code.trim()) code = code.slice(1, -1)
    spans.set(i, { end: close + n, text: code })
    i = close + n
  }
  return spans
}

/** The first of the sorted `list` at or after `from`, or -1. */
function firstFrom(list, from) {
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

/** Each `[`'s matching `]` (nested pairs balanced; not inside code spans, not escaped). */
function bracketPairs(text, spans) {
  const pairs = new Map()
  const open = []
  for (let k = 0; k < text.length; k++) {
    const span = spans.get(k)
    if (span) {
      k = span.end - 1
    } else if (text[k] === '\\') {
      k++
    } else if (text[k] === '[') {
      open.push(k)
    } else if (text[k] === ']' && open.length) {
      pairs.set(open.pop(), k)
    }
  }
  return pairs
}

/** A link's destination after `](`: `(url "title")` — its end and URL, or null. */
function destination(text, at) {
  if (text[at] !== '(') return null
  let j = at + 1
  while (text[j] === ' ' || text[j] === '\t') j++
  let url
  if (text[j] === '<') {
    const end = text.indexOf('>', j)
    if (end < 0 || text.slice(j, end).includes('\n')) return null
    url = text.slice(j + 1, end)
    j = end + 1
  } else {
    const start = j
    let parens = 0
    while (j < text.length) {
      const c = text[j]
      if (SPACE.test(c)) break
      if (c === '\\' && j + 1 < text.length && PUNCTUATION.test(text[j + 1])) {
        j += 2
        continue
      }
      if (c === '(') parens++
      if (c === ')') {
        if (parens === 0) break
        parens--
      }
      j++
    }
    url = text.slice(start, j).replace(/\\([!-/:-@[-`{-~])/g, '$1')
  }
  while (text[j] === ' ' || text[j] === '\t') j++
  const quote = { '"': '"', "'": "'", '(': ')' }[text[j]]
  if (quote) {
    const end = text.indexOf(quote, j + 1)
    if (end < 0) return null
    j = end + 1
    while (text[j] === ' ' || text[j] === '\t') j++
  }
  return text[j] === ')' ? { end: j + 1, url } : null
}

/** A bare https link at `i`: its end, a sentence's stop and unopened `)` left out. */
function autolinkEnd(text, i) {
  if (!text.startsWith('https://', i)) return -1
  let end = i + 8
  while (end < text.length && !SPACE.test(text[end]) && text[end] !== '<') end++
  let opens = 0
  let closes = 0
  for (let k = i; k < end; k++) {
    if (text[k] === '(') opens++
    else if (text[k] === ')') closes++
  }
  for (;;) {
    const c = text[end - 1]
    if (/[.,;:!?'"*_~]/.test(c)) end--
    else if (c === ')' && closes > opens) {
      end--
      closes--
    } else break
  }
  return end > i + 8 ? end : -1
}

function inline(text, depth, links = true) {
  if (depth > MAX_DEPTH) return text ? [{ kind: 'text', text }] : []
  const spans = codeSpans(text)
  const pairs = bracketPairs(text, spans)
  // The tokens, a list: text, line breaks, code, links, and the runs of
  // `*`, `_`, `~~` emphasis is matched among afterwards.
  const tokens = []
  let plain = ''
  const flush = () => {
    if (plain) tokens.push({ kind: 'text', text: plain })
    plain = ''
  }
  let i = 0
  while (i < text.length) {
    const c = text[i]
    const span = spans.get(i)
    if (span) {
      flush()
      tokens.push({ kind: 'code', text: span.text })
      i = span.end
      continue
    }
    if (c === '\\' && i + 1 < text.length && PUNCTUATION.test(text[i + 1])) {
      plain += text[i + 1]
      i += 2
      continue
    }
    if (c === '\n') {
      flush()
      tokens.push({ kind: 'br' })
      i++
      continue
    }
    // An image (`![…](…)`) stays text, whole: its URL never loaded, nor offered.
    if (c === '!' && text[i + 1] === '[' && pairs.has(i + 1)) {
      const close = pairs.get(i + 1)
      const dest = destination(text, close + 1)
      if (dest) {
        plain += text.slice(i, dest.end)
        i = dest.end
        continue
      }
      // Not (yet) whole — still streaming, or a space in its URL: what
      // would be its URL is text too, never a bare link.
      if (text[close + 1] === '(') {
        let j = close + 2
        while (j < text.length && !SPACE.test(text[j]) && text[j] !== ')') j++
        plain += text.slice(i, j)
        i = j
        continue
      }
    }
    if (c === '[' && pairs.has(i)) {
      const close = pairs.get(i)
      const dest = destination(text, close + 1)
      if (dest) {
        const url = links ? linkUrl(dest.url) : null
        if (url) {
          flush()
          const label = text.slice(i + 1, close)
          // Link text with no text shows the URL.
          const parts = label.trim() ? inline(label, depth + 1, false) : [{ kind: 'text', text: url }]
          tokens.push({ kind: 'link', parts, url })
        } else {
          // Another scheme, a URL the page would not open, or a link in a link: text, whole.
          plain += text.slice(i, dest.end)
        }
        i = dest.end
        continue
      }
    }
    if (links && c === 'h' && (i === 0 || !WORD.test(text[i - 1]))) {
      const end = autolinkEnd(text, i)
      if (end > 0 && linkUrl(text.slice(i, end))) {
        flush()
        const url = text.slice(i, end)
        tokens.push({ kind: 'link', parts: [{ kind: 'text', text: url }], url })
        i = end
        continue
      }
    }
    if (c === '*' || c === '_' || c === '~') {
      let n = 1
      while (text[i + n] === c) n++
      if (c === '~' && n !== 2) {
        plain += text.slice(i, i + n)
        i += n
        continue
      }
      const before = i === 0 ? ' ' : text[i - 1]
      const after = i + n >= text.length ? ' ' : text[i + n]
      const left = !SPACE.test(after) && (!PUNCT.test(after) || SPACE.test(before) || PUNCT.test(before))
      const right = !SPACE.test(before) && (!PUNCT.test(before) || SPACE.test(after) || PUNCT.test(after))
      const open = c === '_' ? left && (!right || PUNCT.test(before)) : left
      const closes = c === '_' ? right && (!left || PUNCT.test(after)) : right
      flush()
      tokens.push({ kind: 'delim', char: c, count: n, length: n, open, close: closes })
      i += n
      continue
    }
    plain += c
    i++
  }
  flush()
  return emphasis(tokens, depth)
}

/**
 * CommonMark's emphasis over the tokens (a linked list, so each match
 * splices in constant time; `bottom` keeps a failed look from being made
 * again): `*`/`_` pairs strong (two each) or em, `~~` del; runs left over
 * are text.
 */
function emphasis(tokens, depth) {
  const head = { next: null }
  let last = head
  for (const token of tokens) {
    token.prev = last
    last.next = token
    last = token
  }
  last.next = null
  const delims = tokens.filter((t) => t.kind === 'delim')
  delims.forEach((d, k) => {
    d.below = delims[k - 1] ?? null
    d.above = delims[k + 1] ?? null
  })
  const drop = (d) => {
    if (d.below) d.below.above = d.above
    if (d.above) d.above.below = d.below
    d.dropped = true
  }
  const unlink = (t) => {
    t.prev.next = t.next
    if (t.next) t.next.prev = t.prev
  }
  const bottom = new Map()
  for (let closer = delims[0]; closer; ) {
    if (closer.dropped || !closer.close) {
      closer = closer.above
      continue
    }
    const key = `${closer.char}${closer.open}${closer.length % 3}`
    const floor = bottom.get(key) ?? null
    let opener = closer.below
    while (opener && opener !== floor) {
      const fits =
        opener.char === closer.char &&
        opener.open &&
        !(closer.char !== '~' && (opener.close || closer.open) && (opener.length + closer.length) % 3 === 0 && !(opener.length % 3 === 0 && closer.length % 3 === 0))
      if (fits) break
      opener = opener.below
    }
    if (!opener || opener === floor) {
      bottom.set(key, closer.below)
      const next = closer.above
      if (!closer.open) drop(closer)
      closer = next
      continue
    }
    const use = closer.char === '~' ? 2 : opener.count >= 2 && closer.count >= 2 ? 2 : 1
    const kind = closer.char === '~' ? 'del' : use === 2 ? 'strong' : 'em'
    // What lies between becomes the node's; delimiters there are text now.
    const children = []
    for (let t = opener.next; t !== closer; t = t.next) children.push(t)
    for (let d = opener.above; d && d !== closer; d = d.above) d.dropped = true
    opener.above = closer
    closer.below = opener
    const node = { kind, children, prev: opener, next: closer }
    opener.next = node
    closer.prev = node
    opener.count -= use
    closer.count -= use
    if (opener.count === 0) {
      unlink(opener)
      drop(opener)
    }
    if (closer.count === 0) {
      const next = closer.above
      unlink(closer)
      drop(closer)
      closer = next
    }
  }
  const out = []
  for (let t = head.next; t; t = t.next) out.push(t)
  return parts(out, depth)
}

/** Tokens as parts: runs left over as text, nodes past the depth as text, texts merged. */
function parts(tokens, depth) {
  const out = []
  const text = (s) => {
    if (!s) return
    const prev = out[out.length - 1]
    if (prev?.kind === 'text') prev.text += s
    else out.push({ kind: 'text', text: s })
  }
  const flat = (t) =>
    t.kind === 'text' ? t.text : t.kind === 'delim' ? t.char.repeat(t.count) : t.kind === 'code' ? `\`${t.text}\`` : t.kind === 'br' ? '\n' : t.kind === 'link' ? t.parts.map(flat).join('') : (t.children ?? []).map(flat).join('')
  for (const t of tokens) {
    if (t.kind === 'text') text(t.text)
    else if (t.kind === 'delim') text(t.char.repeat(t.count))
    else if (t.children) {
      if (depth + 1 > MAX_DEPTH) {
        const mark = t.kind === 'del' ? '~~' : t.kind === 'strong' ? '**' : '*'
        text(mark + t.children.map(flat).join('') + mark)
      } else {
        out.push({ kind: t.kind, parts: parts(t.children, depth + 1) })
      }
    } else if (t.kind === 'link') out.push({ kind: 'link', parts: t.parts, url: t.url })
    else if (t.kind === 'code') out.push({ kind: 'code', text: t.text })
    else out.push({ kind: t.kind })
  }
  return out
}

/**
 * Draw `text` as Markdown into `element` with `doc` (the page's
 * `document`): every node made with `createElement`, its text set as text,
 * links checked again by `safeUrl`. A kind it does not know is drawn as its
 * text, never dropped; if drawing throws, the raw text is shown.
 */
export function drawMarkdown(doc, element, text) {
  try {
    element.replaceChildren(...markdownBlocks(text).map((b) => drawBlock(doc, b)))
  } catch (error) {
    console.error('the answer could not be drawn as Markdown', error)
    element.replaceChildren(doc.createTextNode(String(text ?? '')))
  }
}

function drawBlock(doc, b) {
  const el = (tag) => doc.createElement(tag)
  switch (b.kind) {
    case 'heading':
      return drawInline(doc, el(`h${b.level}`), b.inline)
    case 'paragraph':
      return drawInline(doc, el('p'), b.inline)
    case 'list': {
      const list = el(b.ordered ? 'ol' : 'ul')
      if (b.ordered && b.start !== 1) list.start = b.start
      for (const item of b.items) {
        const li = el('li')
        // A tight item (one paragraph) holds its text directly.
        if (item.length === 1 && item[0].kind === 'paragraph') drawInline(doc, li, item[0].inline)
        else li.append(...item.map((x) => drawBlock(doc, x)))
        list.append(li)
      }
      return list
    }
    case 'code': {
      const pre = el('pre')
      if (b.lang) pre.dataset.lang = b.lang
      const code = el('code')
      code.textContent = b.text
      pre.append(code)
      return pre
    }
    case 'quote': {
      const quote = el('blockquote')
      quote.append(...b.blocks.map((x) => drawBlock(doc, x)))
      return quote
    }
    case 'rule':
      return el('hr')
    case 'table': {
      const wrap = el('div')
      wrap.className = 'table'
      const table = el('table')
      const row = (cellParts, tag) => {
        const tr = el('tr')
        cellParts.forEach((cell, at) => {
          const td = drawInline(doc, el(tag), cell)
          if (b.align[at]) td.style.textAlign = b.align[at]
          tr.append(td)
        })
        return tr
      }
      const head = el('thead')
      head.append(row(b.header, 'th'))
      const body = el('tbody')
      body.append(...b.rows.map((r) => row(r, 'td')))
      table.append(head, body)
      wrap.append(table)
      return wrap
    }
    default:
      return doc.createTextNode(JSON.stringify(b))
  }
}

function drawInline(doc, element, inlineParts) {
  for (const part of inlineParts) {
    switch (part.kind) {
      case 'text':
        element.append(doc.createTextNode(part.text))
        break
      case 'br':
        element.append(doc.createElement('br'))
        break
      case 'code': {
        const code = doc.createElement('code')
        code.textContent = part.text
        element.append(code)
        break
      }
      case 'link': {
        const url = safeUrl(part.url)
        if (!url) {
          drawInline(doc, element, part.parts)
          element.append(doc.createTextNode(` (${part.url})`))
          break
        }
        const a = doc.createElement('a')
        drawInline(doc, a, part.parts)
        a.href = url
        a.target = '_blank'
        a.rel = 'noreferrer'
        element.append(a)
        break
      }
      case 'strong':
      case 'em':
      case 'del':
        element.append(drawInline(doc, doc.createElement(part.kind), part.parts))
        break
      default:
        element.append(doc.createTextNode(part.text ?? ''))
    }
  }
  return element
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

// A link target the page may use: http(s) only (Markdown links: https only).
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
