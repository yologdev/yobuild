// git-history: one read-only tool, `release_commits`, for the release-notes agent.
//
// It reads the commits since the last version tag (vX.Y.Z) of the repository
// the agent works in. Read-only by construction: only git subcommands that
// read, no pager, no optional locks (no refreshed index), and the config that
// could run a program or change the output turned off.
import { definePlugin } from '@arcships/rutis'
import { execFile } from 'node:child_process'

const GIT = [
  '--no-pager',
  '--no-optional-locks',
  '-c', 'core.fsmonitor=false',
  '-c', 'log.showSignature=false',
  '-c', 'column.ui=never',
  '-c', 'color.ui=never',
  '-c', 'log.mailmap=false',
]
const ENV = { ...process.env, GIT_NO_LAZY_FETCH: '1', GIT_TERMINAL_PROMPT: '0', GIT_PAGER: 'cat' }

const VERSION = /^v(\d+)\.(\d+)\.(\d+)$/
const MAX_COMMITS = 300        // commits listed at most
const MAX_FILES = 20           // files listed per commit at most
const MAX_BODY = 1500          // characters of a commit's body at most
const MAX_TEXT = 50_000        // characters of the whole answer at most

type Run = { code: number; stdout: string; stderr: string }

function git(args: string[], cwd: string, signal?: AbortSignal): Promise<Run> {
  return new Promise((resolve, reject) => {
    execFile('git', [...GIT, ...args], { cwd, signal, env: ENV, maxBuffer: 16 << 20, encoding: 'utf8' },
      (error: any, stdout: string, stderr: string) => {
        if (error && typeof error.code !== 'number') return reject(error)   // not started, aborted, too much output
        resolve({ code: error ? error.code : 0, stdout, stderr })
      })
  })
}

// git's own reason: its `fatal:` or `error:` line, not a `warning:` before it.
function reason(run: Run): string {
  const lines = run.stderr.split('\n').map(l => l.trim()).filter(Boolean)
  return lines.find(l => /^(fatal|error):/.test(l)) ?? lines[lines.length - 1] ?? `git exited with ${run.code}`
}

class GitFailed extends Error {}
function must(run: Run, what: string): string {
  if (run.code !== 0) throw new GitFailed(`${what} failed: ${reason(run)}`)
  return run.stdout
}

function compare(a: string, b: string): number {
  const x = VERSION.exec(a)!.slice(1).map(Number), y = VERSION.exec(b)!.slice(1).map(Number)
  for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] - y[i]
  return 0
}

async function releaseCommits(cwd: string, since: string | undefined, signal?: AbortSignal) {
  const out: string[] = []

  // A commit at all? `rev-parse --verify -q HEAD` exits 1 with nothing on stderr only when there is none.
  const head = await git(['rev-parse', '--verify', '-q', 'HEAD'], cwd, signal)
  if (head.code === 1 && head.stderr.trim() === '') return { text: 'This repository has no commit yet: there is nothing to draft release notes from.' }
  const headHash = must(head, 'reading HEAD').trim()

  const shallow = must(await git(['rev-parse', '--is-shallow-repository'], cwd, signal), 'asking whether the clone is shallow').trim()
  if (shallow === 'true') out.push('WARNING: this is a shallow clone: part of the history and of the tags may be missing, so the range below may be cut short.')

  // Tags: every tag, and those in HEAD's history.
  const all = must(await git(['tag', '--list', '--format=%(refname:strip=2)'], cwd, signal), 'listing tags').split('\n').filter(Boolean)
  const merged = new Set(must(await git(['tag', '--list', '--merged', 'HEAD', '--format=%(refname:strip=2)'], cwd, signal), 'listing the tags in HEAD\'s history').split('\n').filter(Boolean))
  const versions = all.filter(t => VERSION.test(t) && merged.has(t)).sort(compare).reverse()
  const notVersion = all.filter(t => !VERSION.test(t))
  const notInHead = all.filter(t => VERSION.test(t) && !merged.has(t))

  let from: string | undefined
  if (since !== undefined) {
    const tag = await git(['rev-parse', '--verify', '-q', `refs/tags/${since}^{commit}`], cwd, signal)
    if (tag.code === 1 && tag.stderr.trim() === '') {
      return { text: `There is no tag ${since} in this repository. Version tags in HEAD's history: ${versions.join(', ') || 'none'}.`, is_error: true }
    }
    must(tag, `reading the tag ${since}`)
    if (!merged.has(since)) {
      return { text: `The tag ${since} is not in HEAD's history (it is on another branch): "since ${since}" has no meaning here. Version tags in HEAD's history: ${versions.join(', ') || 'none'}.`, is_error: true }
    }
    from = since
  } else {
    from = versions[0]
  }

  if (from) {
    out.push(`Range: the commits after the tag ${from} up to HEAD (${headHash.slice(0, 7)}), newest first${since === undefined ? ` — ${from} is the highest version tag in HEAD's history` : ' — the tag asked for'}.`)
  } else {
    out.push(`NO VERSION TAG: no tag named like v1.2.3 is in HEAD's history, so the range is every commit from the first one up to HEAD (${headHash.slice(0, 7)}), newest first.`)
  }
  out.push(`Version tags in HEAD's history: ${versions.join(', ') || 'none'}.`)
  if (notVersion.length) out.push(`Tags left out, not named like v1.2.3: ${notVersion.join(', ')}.`)
  if (notInHead.length) out.push(`Version tags left out, not in HEAD's history: ${notInHead.join(', ')}.`)

  // The commits: one NUL-separated record each (a message holds any byte but NUL).
  const range = from ? [`refs/tags/${from}..HEAD`] : ['HEAD']
  const log = must(await git(['log', '-z', '--no-color', `--max-count=${MAX_COMMITS + 1}`, '--date=short',
    '--format=%H%n%P%n%ad%n%an%n%s%n%b', ...range, '--'], cwd, signal), 'reading the commits')
  const records = log.split('\0').filter(r => r.length > 0)
  const cut = records.length > MAX_COMMITS
  const commits: string[] = []
  let unreadable = 0
  for (const record of records.slice(0, MAX_COMMITS)) {
    const lines = record.replace(/^\n/, '').split('\n')
    const [hash, parents, date, author, subject] = lines
    if (lines.length < 5 || !/^[0-9a-f]{40}$/.test(hash ?? '')) { unreadable++; continue }
    let body = lines.slice(5).join('\n').trim()
    if (body.length > MAX_BODY) body = body.slice(0, MAX_BODY) + ` … (body cut at ${MAX_BODY} characters)`
    const merge = parents.trim().split(' ').filter(Boolean).length > 1
    let files: string
    if (merge) {
      files = '(a merge commit: its files are not listed)'
    } else {
      const names = must(await git(['diff-tree', '-r', '-z', '--no-commit-id', '--name-only', '--no-ext-diff', '--no-textconv', '--root', hash], cwd, signal), `listing the files of ${hash.slice(0, 7)}`)
        .split('\0').filter(Boolean)
      files = names.length === 0 ? '(no files changed)'
        : names.slice(0, MAX_FILES).join(', ') + (names.length > MAX_FILES ? `, and ${names.length - MAX_FILES} more` : '')
    }
    let entry = `- ${hash.slice(0, 7)} ${date} ${author}: ${subject}\n  files: ${files}`
    if (body) entry += '\n  body: ' + body.replace(/\n/g, '\n        ')
    commits.push(entry)
  }

  out.push(`Commits: ${commits.length}${cut ? ` (CUT: only the newest ${MAX_COMMITS} are listed; there are more)` : ''}${unreadable ? ` (${unreadable} could not be read and are left out)` : ''}.`)
  if (commits.length === 0 && !unreadable) out.push(`No commit since ${from}: HEAD is the tagged commit.`)
  let text = out.join('\n') + '\n\n' + commits.join('\n')
  if (text.length > MAX_TEXT) {
    const shown = text.slice(0, MAX_TEXT)
    text = shown.slice(0, shown.lastIndexOf('\n- ')) + `\n\n(CUT: the answer was longer than ${MAX_TEXT} characters; the oldest commits are left out.)`
  }
  return { text }
}

export default definePlugin({
  inject: ['yoagent', 'host_workspace'],
  apply(ctx: any) {
    ctx.effect(ctx.use('yoagent').register('git-history', {
      async tools() {
        return [{
          name: 'release_commits',
          description: 'The commits since the last version tag (vX.Y.Z) of the repository the agent works in, newest first: '
            + 'each with its hash, date, author, subject, body and changed files. Without `since`, it takes the highest '
            + 'version tag in HEAD\'s history, and says so; with no such tag, every commit. It also lists the tags it left out. Read-only.',
          parameters: {
            type: 'object',
            properties: {
              since: { type: 'string', description: 'A version tag (v1.2.3) to start after, instead of the last one.' },
            },
            additionalProperties: false,
          },
        }]
      },
      async call_tool(call: any) {
        if (call.tool !== 'release_commits') return { text: `unknown tool ${JSON.stringify(call.tool)}`, is_error: true }
        const args = call.args ?? {}
        if (typeof args !== 'object' || Array.isArray(args)) return { text: 'the arguments must be an object', is_error: true }
        const extra = Object.keys(args).filter(k => k !== 'since')
        if (extra.length) return { text: `unknown argument(s): ${extra.join(', ')} (it takes only \`since\`)`, is_error: true }
        const since = args.since
        if (since !== undefined && since !== null && (typeof since !== 'string' || !VERSION.test(since))) {
          return { text: `not a version tag (v1.2.3): ${JSON.stringify(since)}`, is_error: true }
        }
        const cwd = await ctx.use('host_workspace').cwd()
        try {
          return await releaseCommits(cwd, since ?? undefined, call.signal)
        } catch (e: any) {
          if (e instanceof GitFailed) return { text: e.message, is_error: true }
          throw e
        }
      },
    }))
  },
})
