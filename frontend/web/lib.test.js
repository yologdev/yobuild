// node --test web/
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { jsonText, MAX_DEPTH, markdownBlocks, markdownParts, Questions, runEndLine } from './lib.js'

test('a value with a big integer still reads as JSON text', () => {
  assert.equal(jsonText({ id: 2n ** 64n, n: 1 }), '{"id":"18446744073709551616","n":1}')
  assert.equal(jsonText('x'), '"x"')
  assert.equal(jsonText(1n), '"1"')
})

const text = (t) => ({ kind: 'text', text: t })
const para = (...inline) => ({ kind: 'paragraph', inline })

test('markdown reads inline marks into nested parts', () => {
  assert.deepEqual(markdownParts('a **b *c*** `d` ~~e~~ _f_ [g](https://e.f/g) h'), [
    text('a '),
    { kind: 'strong', parts: [text('b '), { kind: 'em', parts: [text('c')] }] },
    text(' '),
    { kind: 'code', text: 'd' },
    text(' '),
    { kind: 'del', parts: [text('e')] },
    text(' '),
    { kind: 'em', parts: [text('f')] },
    text(' '),
    { kind: 'link', text: 'g', url: 'https://e.f/g' },
    text(' h'),
  ])
  // A line break is kept; code spans of any run; escapes; bare https links,
  // a sentence's closing parenthesis and stop left out.
  assert.deepEqual(markdownParts('x\ny'), [text('x'), { kind: 'br' }, text('y')])
  assert.deepEqual(markdownParts('``a`b``'), [{ kind: 'code', text: 'a`b' }])
  assert.deepEqual(markdownParts('\\*not\\* 2 * 3'), [text('*not* 2 * 3')])
  assert.deepEqual(markdownParts('(see https://a.b/c_(d).)'), [
    text('(see '),
    { kind: 'link', text: 'https://a.b/c_(d)', url: 'https://a.b/c_(d)' },
    text('.)'),
  ])
  // `_` inside a word is a word's; unclosed marks are text.
  assert.deepEqual(markdownParts('snake_case_name'), [text('snake_case_name')])
  assert.deepEqual(markdownParts('**open `tick'), [text('**open `tick')])
})

test('markdown makes no link of other schemes, and no HTML or image of anything', () => {
  for (const t of ['[x](javascript:alert(1))', '[x](http://e.f)', '<img src=x onerror=alert(1)>', '[x](data:text/html,hi)', 'ftp://e.f', '![i](https://e.f/i.png)']) {
    const parts = markdownParts(t)
    assert.ok(parts.every((p) => p.kind === 'text' || (p.kind === 'link' && p.url.startsWith('https://'))), t)
    assert.equal(parts.map((p) => p.text).join(''), t)
  }
  // An image is text, whole: never a link to its URL.
  assert.deepEqual(markdownParts('a ![i](https://e.f/i.png) b'), [text('a ![i](https://e.f/i.png) b')])
  assert.deepEqual(markdownParts(''), [])
})

test('markdown reads headings, paragraphs, rules, quotes and code', () => {
  assert.deepEqual(markdownBlocks('## Done ##\nfirst\nsecond\n\n#nope\n\n---\n> quoted\n> **on**\n\n```js\nx = 1\n\n```\n'), [
    { kind: 'heading', level: 2, inline: [text('Done')] },
    para(text('first'), { kind: 'br' }, text('second')),
    para(text('#nope')),
    { kind: 'rule' },
    { kind: 'quote', blocks: [para(text('quoted'), { kind: 'br' }, { kind: 'strong', parts: [text('on')] })] },
    { kind: 'code', lang: 'js', text: 'x = 1\n' },
  ])
  // An unclosed fence (a reply still streaming) runs to the end.
  assert.deepEqual(markdownBlocks('~~~\na\n# b'), [{ kind: 'code', lang: '', text: 'a\n# b' }])
})

test('markdown reads lists, nested and loose, numbered from their start', () => {
  assert.deepEqual(markdownBlocks('- a\n  - b\n    c\n- d\n\n3. x\n4) y\n\n   more\n'), [
    {
      kind: 'list',
      ordered: false,
      start: 1,
      items: [
        [para(text('a')), { kind: 'list', ordered: false, start: 1, items: [[para(text('b'), { kind: 'br' }, text('c'))]] }],
        [para(text('d'))],
      ],
    },
    // `.` and `)` are two lists; the blank line and the indented paragraph stay in the item.
    { kind: 'list', ordered: true, start: 3, items: [[para(text('x'))]] },
    { kind: 'list', ordered: true, start: 4, items: [[para(text('y')), para(text('more'))]] },
  ])
  // A paragraph line not indented continues the item (lazily).
  assert.deepEqual(markdownBlocks('- a\nb'), [{ kind: 'list', ordered: false, start: 1, items: [[para(text('a'), { kind: 'br' }, text('b'))]] }])
})

test('markdown reads GFM tables with their alignments, padding short rows and cutting long ones', () => {
  assert.deepEqual(markdownBlocks('| A | B | C |\n|:--|--:|:-:|\n| 1 | **2** |\n| x \\| y | 5 | 6 | 7 |\nafter'), [
    {
      kind: 'table',
      align: ['left', 'right', 'center'],
      header: [[text('A')], [text('B')], [text('C')]],
      rows: [
        [[text('1')], [{ kind: 'strong', parts: [text('2')] }], []],
        [[text('x | y')], [text('5')], [text('6')]],
      ],
    },
    para(text('after')),
  ])
  // No separator, or one of another width: not a table.
  assert.equal(markdownBlocks('a | b\nc | d')[0].kind, 'paragraph')
  assert.equal(markdownBlocks('| a | b |\n|---|\n')[0].kind, 'paragraph')
})

test('markdown nesting past its depth is text, and reading stays fast', () => {
  const deep = markdownBlocks('>'.repeat(MAX_DEPTH + 5) + ' x')
  let d = 0
  let b = deep[0]
  while (b.kind === 'quote') {
    b = b.blocks[0]
    d++
  }
  assert.ok(d <= MAX_DEPTH + 1 && b.kind === 'paragraph', JSON.stringify(deep))
  // Many openers with no closer: no quadratic scan.
  const started = Date.now()
  markdownBlocks('*a '.repeat(20000) + '[x '.repeat(20000) + '`'.repeat(5000) + ' x')
  assert.ok(Date.now() - started < 2000, `${Date.now() - started} ms`)
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
