// node --test web/
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { drawMarkdown, jsonText, MAX_DEPTH, markdownBlocks, markdownParts, Questions, runEndLine, TERMINAL_SEQUENCE_MAX, terminalText } from './lib.js'

test('a value with a big integer still reads as JSON text', () => {
  assert.equal(jsonText({ id: 2n ** 64n, n: 1 }), '{"id":"18446744073709551616","n":1}')
  assert.equal(jsonText('x'), '"x"')
  assert.equal(jsonText(1n), '"1"')
})

const text = (t) => ({ kind: 'text', text: t })
const para = (...inline) => ({ kind: 'paragraph', inline })
const link = (url, ...parts) => ({ kind: 'link', parts: parts.length ? parts : [text(url)], url })
const em = (...parts) => ({ kind: 'em', parts })
const strong = (...parts) => ({ kind: 'strong', parts })
const code = (t) => ({ kind: 'code', text: t })
const br = { kind: 'br' }

test('markdown reads inline marks into nested parts', () => {
  assert.deepEqual(markdownParts('a **b *c*** `d` ~~e~~ _f_ [g](https://e.f/g) h'), [
    text('a '),
    strong(text('b '), em(text('c'))),
    text(' '),
    code('d'),
    text(' '),
    { kind: 'del', parts: [text('e')] },
    text(' '),
    em(text('f')),
    text(' '),
    link('https://e.f/g', text('g')),
    text(' h'),
  ])
  // Emphasis around strong (an italic note with a bold word).
  assert.deepEqual(markdownParts('*a **b** c*'), [em(text('a '), strong(text('b')), text(' c'))])
  assert.deepEqual(markdownParts('*Note: **x***'), [em(text('Note: '), strong(text('x')))])
  assert.deepEqual(markdownParts('_a **b** c_'), [em(text('a '), strong(text('b')), text(' c'))])
  assert.deepEqual(markdownParts('__a__ x__y__'), [strong(text('a')), text(' x__y__')])
  // A line break kept; code spans of any run, backslashes literal inside.
  assert.deepEqual(markdownParts('x\ny'), [text('x'), br, text('y')])
  assert.deepEqual(markdownParts('``a`b``'), [code('a`b')])
  assert.deepEqual(markdownParts('Run `C:\\Users\\` now'), [text('Run '), code('C:\\Users\\'), text(' now')])
  assert.deepEqual(markdownParts('the `\\` key'), [text('the '), code('\\'), text(' key')])
  assert.deepEqual(markdownParts('\\*not\\* 2 * 3'), [text('*not* 2 * 3')])
  // Bare https links: a sentence's stop and closing parenthesis left out.
  assert.deepEqual(markdownParts('(see https://a.b/c_(d).)'), [text('(see '), link('https://a.b/c_(d)'), text('.)')])
  // `_` inside a word is the word's; unclosed marks are text.
  assert.deepEqual(markdownParts('snake_case_name'), [text('snake_case_name')])
  assert.deepEqual(markdownParts('**open `tick'), [text('**open `tick')])
  // Code spans and links come first: a mark inside them closes nothing.
  assert.deepEqual(markdownParts('*see [l](https://a.com/x*y) end*'), [em(text('see '), link('https://a.com/x*y', text('l')), text(' end'))])
  assert.deepEqual(markdownParts('*a `x*` b*'), [em(text('a '), code('x*'), text(' b'))])
})

test('markdown links: their text read, balanced parentheses and titles, an empty text shows the URL', () => {
  assert.deepEqual(markdownParts('[`api` and **x**](https://e.f)'), [link('https://e.f', code('api'), text(' and '), strong(text('x')))])
  assert.deepEqual(markdownParts('[a](https://e.f/a_(b))'), [link('https://e.f/a_(b)', text('a'))])
  assert.deepEqual(markdownParts('[x](https://e.f "t")'), [link('https://e.f', text('x'))])
  assert.deepEqual(markdownParts('[](https://e.f)'), [link('https://e.f')])
  assert.deepEqual(markdownParts('[text](https://e.f'), [text('[text]('), link('https://e.f')])
})

test('markdown makes no link of other schemes, and no HTML or image of anything', () => {
  for (const t of [
    '[x](javascript:alert(1))',
    '[x](http://e.f)',
    '<img src=x onerror=alert(1)>',
    '[x](data:text/html,hi)',
    'ftp://e.f',
    '![i](https://e.f/i.png)',
    '![x](https://e.com/i.png "title")',
    '[https://a.b](javascript:x)',
    '[x](https://exa%zzmple.com)',
    '![x](https://e.com/a b.png)',
  ]) {
    assert.deepEqual(markdownParts(t), [text(t)], t)
  }
  // An image is text, whole — in a link too, and after an escaped backslash.
  assert.deepEqual(markdownParts('[![i](https://e.f/i.png)](https://e.f)'), [link('https://e.f', text('![i](https://e.f/i.png)'))])
  assert.deepEqual(markdownParts('\\\\![i](https://e.f/i.png)'), [text('\\![i](https://e.f/i.png)')])
  // An escaped `!` is no image: the link is a link.
  assert.deepEqual(markdownParts('\\![i](https://e.f)'), [text('!'), link('https://e.f', text('i'))])
  assert.deepEqual(markdownParts(''), [])
  assert.deepEqual(markdownBlocks(null), [])
})

test('markdown reads headings, paragraphs, rules, quotes and code', () => {
  assert.deepEqual(markdownBlocks('## Done ##\nfirst\nsecond\n\n#nope\n\n---\n> quoted\n> **on**\n\n```js {1}\nx = 1\n\n```\n'), [
    { kind: 'heading', level: 2, inline: [text('Done')] },
    para(text('first'), br, text('second')),
    para(text('#nope')),
    { kind: 'rule' },
    { kind: 'quote', blocks: [para(text('quoted'), br, strong(text('on')))] },
    { kind: 'code', lang: 'js', text: 'x = 1\n' },
  ])
  assert.deepEqual(markdownBlocks('# C# #\n####### x\n#'), [
    { kind: 'heading', level: 1, inline: [text('C#')] },
    para(text('####### x')),
    { kind: 'heading', level: 1, inline: [] },
  ])
  assert.deepEqual(markdownBlocks('***\n___\n- - -').map((b) => b.kind), ['rule', 'rule', 'rule'])
  // An unclosed fence (a reply still streaming) runs to the end.
  assert.deepEqual(markdownBlocks('~~~\na\n# b'), [{ kind: 'code', lang: '', text: 'a\n# b' }])
  // A one-line ``` with more backticks after it is no fence: nothing swallowed.
  assert.deepEqual(markdownBlocks('```js``` is how you start\nmore'), [para(code('js'), text(' is how you start'), br, text('more'))])
  // CRLF and a lone CR.
  assert.deepEqual(markdownBlocks('# h\r\n- a\r\n- b\r\n```\r\nc\r\n```'), [
    { kind: 'heading', level: 1, inline: [text('h')] },
    { kind: 'list', ordered: false, start: 1, items: [[para(text('a'))], [para(text('b'))]] },
    { kind: 'code', lang: '', text: 'c' },
  ])
  // A heading, a list, a table interrupt a paragraph; an empty marker, or a
  // number other than 1, does not (`2024.` is the paragraph's).
  assert.deepEqual(markdownBlocks('para\n# H').map((b) => b.kind), ['paragraph', 'heading'])
  assert.deepEqual(markdownBlocks('para\n- a').map((b) => b.kind), ['paragraph', 'list'])
  assert.deepEqual(markdownBlocks('para\n-\nnext'), [para(text('para'), br, text('-'), br, text('next'))])
  assert.deepEqual(markdownBlocks('the year\n2024. was good'), [para(text('the year'), br, text('2024. was good'))])
  assert.deepEqual(markdownBlocks('intro\n| a | b |\n|---|---|\n| 1 | 2 |\n\nafter').map((b) => b.kind), ['paragraph', 'table', 'paragraph'])
})

test('markdown reads lists, nested and loose, numbered from their start', () => {
  assert.deepEqual(markdownBlocks('- a\n  - b\n    c\n- d\n\n3. x\n4) y\n\n   more\n'), [
    {
      kind: 'list',
      ordered: false,
      start: 1,
      items: [
        [para(text('a')), { kind: 'list', ordered: false, start: 1, items: [[para(text('b'), br, text('c'))]] }],
        [para(text('d'))],
      ],
    },
    // `.` and `)` are two lists; the blank line and the indented paragraph stay in the item.
    { kind: 'list', ordered: true, start: 3, items: [[para(text('x'))]] },
    { kind: 'list', ordered: true, start: 4, items: [[para(text('y')), para(text('more'))]] },
  ])
  // A paragraph line not indented continues the item (lazily).
  assert.deepEqual(markdownBlocks('- a\nb'), [{ kind: 'list', ordered: false, start: 1, items: [[para(text('a'), br, text('b'))]] }])
  // A fence in an item: steps with their commands.
  assert.deepEqual(markdownBlocks('1. step\n   ```sh\n   ls\n   ```\n2. next'), [
    { kind: 'list', ordered: true, start: 1, items: [[para(text('step')), { kind: 'code', lang: 'sh', text: 'ls' }], [para(text('next'))]] },
  ])
  // Not indented: the list splits, its numbering kept.
  assert.deepEqual(markdownBlocks('1. step\n```sh\nls\n```\n2. next').map((b) => b.kind === 'list' ? `list@${b.start}` : b.kind), ['list@1', 'code', 'list@2'])
  // An open fence in an item takes no lazy line.
  assert.deepEqual(markdownBlocks('- ```\n  code\nnot code').map((b) => b.kind), ['list', 'paragraph'])
  // Tabs, a start of 0, an empty item, other markers make other lists.
  assert.deepEqual(markdownBlocks('-\ta\n\t- b'), [{ kind: 'list', ordered: false, start: 1, items: [[para(text('a')), { kind: 'list', ordered: false, start: 1, items: [[para(text('b'))]] }]] }])
  assert.equal(markdownBlocks('0. a\n1. b')[0].start, 0)
  assert.deepEqual(markdownBlocks('- \n- b')[0].items, [[], [para(text('b'))]])
  assert.equal(markdownBlocks('1. a\n- b').length, 2)
  assert.equal(markdownBlocks('- a\n* b').length, 2)
  assert.deepEqual(markdownBlocks('> - a\n> - b'), [{ kind: 'quote', blocks: [{ kind: 'list', ordered: false, start: 1, items: [[para(text('a'))], [para(text('b'))]] }] }])
})

test('markdown reads GFM tables: alignments, short rows padded, long rows kept, pipes in code', () => {
  assert.deepEqual(markdownBlocks('| A | B | C |\n|:--|--:|:-:|\n| 1 | **2** |\n| x \\| y | 5 | 6 | 7 |\nafter'), [
    {
      kind: 'table',
      align: ['left', 'right', 'center'],
      header: [[text('A')], [text('B')], [text('C')]],
      rows: [
        [[text('1')], [strong(text('2'))], []],
        [[text('x | y')], [text('5')], [text('6 | 7')]],
      ],
    },
    para(text('after')),
  ])
  assert.deepEqual(markdownBlocks('| cmd | what |\n|---|---|\n| `a || b` | either |')[0].rows, [[[code('a || b')], [text('either')]]])
  // A table of piped rows ends at a line that is not one.
  assert.deepEqual(markdownBlocks('| a |\n|---|\n| 1 |\nfoo | bar').map((b) => b.kind), ['table', 'paragraph'])
  // Escaped pipes in the header; a separator not yet complete (streaming).
  assert.deepEqual(markdownBlocks('| a | b \\|\n|---|---|')[0].header, [[text('a')], [text('b |')]])
  assert.equal(markdownBlocks('| a | b |\n|---|')[0].kind, 'paragraph')
  assert.deepEqual(markdownBlocks('| a | b |\n|---|---|\n| 1')[0].rows, [[[text('1')], []]])
  // No separator, or one of another width: not a table.
  assert.equal(markdownBlocks('a | b\nc | d')[0].kind, 'paragraph')
  assert.equal(markdownBlocks('| a | b |\n|---|\n')[0].kind, 'paragraph')
})

test('markdown nesting past its depth is text, the text kept', () => {
  const deep = markdownBlocks('>'.repeat(MAX_DEPTH + 5) + ' x')
  let d = 0
  let b = deep[0]
  while (b.kind === 'quote') {
    b = b.blocks[0]
    d++
  }
  assert.equal(d, MAX_DEPTH + 1, JSON.stringify(deep))
  assert.match(b.inline[0].text, /x$/)
  const lists = markdownBlocks(Array.from({ length: 20 }, (_, k) => '  '.repeat(k) + '- x').join('\n'))
  let levels = 0
  let at = lists
  while (at[0]?.kind === 'list') {
    levels++
    const item = at[0].items[0]
    at = item.filter((x) => x.kind === 'list').length ? item.filter((x) => x.kind === 'list') : item
  }
  assert.ok(levels <= MAX_DEPTH + 1, `${levels}`)
  assert.match(JSON.stringify(lists), /- x/)
})

test('markdown reading stays fast on hostile text', () => {
  const cases = {
    openers: '*a '.repeat(20000) + '_a '.repeat(20000) + '**a '.repeat(20000) + '[x '.repeat(20000) + Array.from({ length: 20000 }, (_, k) => '`'.repeat((k % 7) + 1) + 'a').join(' '),
    heading: '# a' + ' '.repeat(40000) + 'b',
    separator: 'a|b\n---' + ' '.repeat(40000) + 'x',
    parens: 'https://a.b/' + ')'.repeat(40000),
    stops: 'https://a.b/' + '.)'.repeat(20000),
    padding: '` ' + 'a'.repeat(40000) + '`',
  }
  for (const [name, input] of Object.entries(cases)) {
    const started = Date.now()
    markdownBlocks(input)
    assert.ok(Date.now() - started < 1000, `${name}: ${Date.now() - started} ms`)
  }
})

test('markdown reads every prefix of a streaming answer without a throw or an unsafe link', () => {
  const answer = '## File read\n- `expenses.csv` — **8 rows**, see [docs](https://e.f/a_(b)) and https://e.f.\n  - *nested* ~~x~~\n\n| A | B |\n|:--|--:|\n| 1 | 2 |\n\n1. step\n   ```sh\n   ls | wc\n   ```\n\n> quote ![i](https://e.f/i.png)\n'
  const links = (x) => (Array.isArray(x) ? x.flatMap(links) : x && typeof x === 'object' ? [...(x.kind === 'link' ? [x.url] : []), ...Object.values(x).flatMap(links)] : [])
  for (let k = 0; k <= answer.length; k++) {
    const blocks = markdownBlocks(answer.slice(0, k))
    for (const url of links(blocks)) assert.ok(url.startsWith('https://') && !url.includes('i.png'), `${k}: ${url}`)
  }
})

// A document good enough to draw into: elements with a tag, children,
// attributes; text set as text.
function fakeDocument() {
  const node = (tag) => ({
    tag,
    children: [],
    style: {},
    dataset: {},
    append(...c) {
      this.children.push(...c)
    },
    replaceChildren(...c) {
      this.children = c
    },
    set textContent(v) {
      this.children = [{ text: String(v) }]
    },
  })
  return { createElement: node, createTextNode: (t) => ({ text: t }) }
}
const textOf = (n) => n.text ?? n.children.map(textOf).join('')
const find = (n, tag) => (n.tag === tag ? [n] : (n.children ?? []).flatMap((c) => find(c, tag)))

test("an answer is drawn node by node, links and numbers as read, the model's text only as text", () => {
  const doc = fakeDocument()
  const root = doc.createElement('div')
  drawMarkdown(doc, root, '3. a\n\n0) b\n\n1. c\n\n- tight\n\n- loose\n\n  more\n\n| x |\n|--:|\n| 1 |\n\n```js\nx\n```\n\n[a](https://e.f) <img onerror=alert(1)>')
  const [three, zero, one] = find(root, 'ol')
  assert.equal(three.start, 3)
  assert.equal(zero.start, 0)
  assert.equal(one.start, undefined)
  const [tight, loose] = find(root, 'ul')[0].children
  assert.deepEqual(tight.children.map((c) => c.tag ?? 'text'), ['text'])
  assert.deepEqual(loose.children.map((c) => c.tag), ['p', 'p'])
  assert.equal(find(root, 'td')[0].style.textAlign, 'right')
  assert.equal(find(root, 'pre')[0].dataset.lang, 'js')
  const [a] = find(root, 'a')
  assert.deepEqual([a.href, a.target, a.rel, textOf(a)], ['https://e.f/', '_blank', 'noreferrer', 'a'])
  assert.match(textOf(root), /<img onerror=alert\(1\)>/)
  assert.equal(find(root, 'img').length, 0)
  // Nothing to draw: nothing drawn.
  const other = doc.createElement('p')
  drawMarkdown(doc, other, '')
  assert.deepEqual(other.children, [])
})

test('the page draws answers only through drawMarkdown: no HTML from strings anywhere', () => {
  const app = readFileSync(new URL('./app.js', import.meta.url), 'utf8')
  for (const sink of ['innerHTML', 'outerHTML', 'insertAdjacentHTML', 'document.write', 'DOMParser']) {
    assert.ok(!app.includes(sink), sink)
  }
})

test('terminal text loses every escape sequence and control, and keeps the text', () => {
  const cases = [
    ['a\x1b[2Jb\x1b[31mred\x1b[0m', 'abred'],
    ['a\x1b]0;PWNED\x07b', 'ab'],
    ['a\x1b]8;;https://evil\x1b\\click\x1b]8;;\x1b\\b', 'aclickb'],
    ['a\x1b]52;c;aGk=\x07b', 'ab'],
    ['a\x1bPq#0\x1b\\b \x1b_x\x1b\\c \x1b^y\x1b\\d \x1bXz\x1b\\e', 'ab c d e'],
    ['a\x1b(Bb\x1bcc', 'abc'],
    ['a\x07b\x08c\rd\x7fe\x00f', 'abcdef'],
    ['a\u009b2Jb\u009d0;t\u0007c', 'a2Jb0;tc'],
    ['a‮evil‬⁦x⁩‎‏؜b', 'aevilxb'],
    ['tab\tand\nline, ünï 中文 😀', 'tab\tand\nline, ünï 中文 😀'],
    ['end \x1b', 'end '],
  ]
  for (const [input, want] of cases) assert.equal(terminalText(input), want, JSON.stringify(input))
  assert.equal(terminalText(undefined), '')
  // Unterminated: it ends at the next control — the line after is shown —
  // or after the bound; only the introducer goes.
  assert.equal(terminalText('a\x1b]0;title\nnext line'), 'a0;title\nnext line')
  const long = 'x'.repeat(TERMINAL_SEQUENCE_MAX + 10)
  assert.equal(terminalText(`\x1b]0;${long}`), `0;${long}`)
  // Pieces cleaned apart join clean (an answer's deltas).
  const pieces = ['ok \x1b]0;', 'T\x07 done', '\x1b[2', 'J!']
  const joined = pieces.map(terminalText).join('')
  assert.ok(!/[\x00-\x08\x0b-\x1f\x7f\u0080-\u009f]/.test(joined), JSON.stringify(joined))
})

test('questions show oldest first, once each', () => {
  const q = new Questions()
  assert.equal(q.add(1, { kind: 'confirm' }), true)
  assert.equal(q.add(2, { kind: 'select' }), true)
  assert.equal(q.add(1, { kind: 'confirm' }), false, 'a repeat from hello')
  assert.equal(q.current().id, 1)
  assert.equal(q.remove(2), false, 'not the one shown')
  assert.equal(q.current().id, 1)
  assert.equal(q.remove(1), true)
  assert.equal(q.current(), undefined)
  assert.equal(q.remove(9), false)
})

test('a run line says how the run ended', () => {
  assert.equal(runEndLine({ run: 1, outcome: 'completed', totalCostUsd: 0.01234 }), 'run 1 done · $0.0123')
  assert.equal(runEndLine({ run: 2, outcome: 'aborted', error: 'aborted' }), 'run 2 stopped')
  assert.equal(runEndLine({ run: 3, outcome: 'rejected', error: 'input rejected: no' }), 'run 3 refused: input rejected: no')
  assert.equal(runEndLine({ run: 4, outcome: 'error', error: 'overloaded' }), 'run 4 failed: overloaded')
  assert.equal(
    runEndLine({ run: 5, outcome: 'stopped', error: '[Agent stopped: max turns (1) reached]' }),
    'run 5 stopped early: [Agent stopped: max turns (1) reached]',
  )
})

import { diffRows, safeUrl, toolView, viewTitle } from './lib.js'

test('a tool view is read only in the card vocabulary', () => {
  assert.equal(toolView(undefined), undefined)
  assert.equal(toolView({ view: { call: { card: 'dance' } } }), undefined)
  const view = toolView({ view: { call: { card: 'generic', title: 'Echo hi' }, result: { card: 'terminal', output: 'hi' } } })
  assert.equal(view.call.title, 'Echo hi')
  assert.equal(viewTitle(view), 'Echo hi', 'the result has no title of its own')
  assert.equal(viewTitle({ call: { title: 'a' }, result: { title: 'b' } }), 'b')
})

test('a diff keeps common lines and marks the rest', () => {
  const ops = (rows) => rows.map((r) => r.op + r.text).join('|')
  assert.equal(ops(diffRows('a\nb\nc', 'a\nx\nc')), ' a|-b|+x| c')
  assert.equal(ops(diffRows(null, 'new')), '+new', 'a new file')
  assert.equal(ops(diffRows('a\nb', 'a\nb\nc', 1)), '-a|-b|+a|+b|+c', 'too big to align')
})

test('only http(s) links', () => {
  assert.equal(safeUrl('https://example.com/a'), 'https://example.com/a')
  assert.equal(safeUrl('http://example.com/'), 'http://example.com/')
  for (const bad of ['javascript:alert(1)', 'data:text/html,x', 'not a url', undefined]) assert.equal(safeUrl(bad), undefined)
})

import { parseAnswer } from './lib.js'

test('a typed answer fits its question, or is asked again', () => {
  const one = { kind: 'select', options: ['a', 'b', 'c'] }
  const many = { ...one, multiple: true }
  assert.deepEqual(parseAnswer(one, ' 2 '), { value: 'b' })
  assert.deepEqual(parseAnswer(one, ''), { value: null }, 'blank: no choice')
  assert.ok(parseAnswer(one, '9').error)
  assert.ok(parseAnswer(one, '1.5').error)
  assert.deepEqual(parseAnswer(many, '3, 1,1'), { value: ['c', 'a'] })
  assert.deepEqual(parseAnswer(many, ''), { value: [] }, 'blank: none')
  assert.match(parseAnswer(many, '1,5').error, /"5"/)
  assert.ok(parseAnswer(many, 'x').error)
  assert.deepEqual(parseAnswer({ kind: 'confirm' }, 'Yes'), { value: true })
  assert.deepEqual(parseAnswer({ kind: 'confirm' }, 'n'), { value: false })
  assert.ok(parseAnswer({ kind: 'confirm' }, 'maybe').error)
  assert.deepEqual(parseAnswer({ kind: 'input' }, '  spaced '), { value: '  spaced ' })
})
