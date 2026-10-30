// release-history: reads a git repository's version tags and the commits
// since one, for a read-only release-notes agent. It only reads: git is run
// with fixed arguments, --no-optional-locks (no index refresh, no lock file)
// and no subcommand that writes.
import { definePlugin } from '@arcships/rutis'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'

const run = promisify(execFile)

const VERSION_TAG = /^v\d+\.\d+\.\d+$/
const MAX_COMMITS = 500

async function git(cwd: string, args: string[]): Promise<string> {
  const { stdout } = await run(
    'git',
    ['--no-pager', '--no-optional-locks', '-c', 'log.showSignature=false', ...args],
    { cwd, maxBuffer: 16 * 1024 * 1024, env: { ...process.env, GIT_TERMINAL_PROMPT: '0' } },
  )
  return stdout
}

function failed(e: any): { text: string; is_error: true } {
  const said = String(e?.stderr || e?.message || e).trim().split('\n')[0]
  return { text: `git failed: ${said}`, is_error: true }
}

async function versionTags(cwd: string) {
  let head: string
  try {
    head = (await git(cwd, ['rev-parse', '--verify', '-q', 'HEAD'])).trim()
  } catch (e: any) {
    if (e?.code === 1) return { text: 'no commits yet: there is nothing to release' }
    return failed(e)
  }
  if (!head) return { text: 'no commits yet: there is nothing to release' }
  let out: string
  try {
    out = await git(cwd, ['tag', '--merged', 'HEAD', '--sort=-v:refname', '--list', 'v[0-9]*'])
  } catch (e) {
    return failed(e)
  }
  const tags = out.split('\n').map((t) => t.trim()).filter((t) => VERSION_TAG.test(t))
  if (tags.length === 0) {
    return { text: 'no version tag (vX.Y.Z) in the history of HEAD: every commit is unreleased' }
  }
  return { text: `latest version tag: ${tags[0]}\nall version tags in HEAD's history, newest first:\n${tags.join('\n')}` }
}

async function commitsSince(cwd: string, since: string | undefined) {
  const range = since ? [`refs/tags/${since}..HEAD`] : ['HEAD']
  let out: string
  try {
    out = await git(cwd, [
      'log', '--no-color', '--no-merges', `--max-count=${MAX_COMMITS + 1}`,
      '--format=%x1e%h%x00%an%x00%as%x00%s%x00%b%x00', '--name-only', ...range, '--',
    ])
  } catch (e) {
    return failed(e)
  }
  const records = out.split('\x1e').filter((r) => r.trim() !== '')
  const cut = records.length > MAX_COMMITS
  const lines: string[] = []
  for (const r of records.slice(0, MAX_COMMITS)) {
    const [hash, author, date, subject, body, rest] = r.split('\x00')
    lines.push(`commit ${hash} (${date}, ${author}): ${subject}`)
    const b = (body ?? '').trim()
    if (b) lines.push(...b.split('\n').map((l) => `    ${l}`))
    const files = (rest ?? '').split('\n').map((f) => f.trim()).filter(Boolean)
    if (files.length) lines.push(`  files: ${files.join(', ')}`)
  }
  const what = since ? `since ${since}` : 'in all of HEAD\'s history (no tag given)'
  if (records.length === 0) return { text: `no commits ${what} (merge commits left out)` }
  const head = `${Math.min(records.length, MAX_COMMITS)} commit(s) ${what}, newest first, merge commits left out${cut ? `; only the newest ${MAX_COMMITS} shown` : ''}:`
  return { text: [head, ...lines].join('\n') }
}

export default definePlugin({
  inject: ['yoagent', 'host_workspace'],
  apply(ctx: any) {
    ctx.effect(ctx.use('yoagent').register('release-history', {
      async tools() {
        return [
          {
            name: 'version_tags',
            description: 'The version tags (vX.Y.Z) in the history of HEAD, newest first; says so when there is none.',
            parameters: { type: 'object', properties: {}, additionalProperties: false },
          },
          {
            name: 'commits_since',
            description: 'The commits since a version tag (or every commit when `since` is left out), newest first, merges left out: hash, date, author, subject, body and the files each changed.',
            parameters: {
              type: 'object',
              properties: { since: { type: 'string', description: 'a version tag, vX.Y.Z; leave out when there is none' } },
              additionalProperties: false,
            },
          },
        ]
      },
      async call_tool(call: any) {
        const args = call.args ?? {}
        if (typeof args !== 'object' || Array.isArray(args)) {
          return { text: 'arguments must be an object', is_error: true }
        }
        const cwd = await ctx.use('host_workspace').cwd()
        if (call.tool === 'version_tags') {
          if (Object.keys(args).length) return { text: 'version_tags takes no arguments', is_error: true }
          return versionTags(cwd)
        }
        if (call.tool === 'commits_since') {
          const extra = Object.keys(args).filter((k) => k !== 'since')
          if (extra.length) return { text: `unknown argument: ${extra.join(', ')}`, is_error: true }
          const since = args.since
          if (since !== undefined && since !== null && (typeof since !== 'string' || !VERSION_TAG.test(since))) {
            return { text: `not a version tag (vX.Y.Z): ${JSON.stringify(since)}`, is_error: true }
          }
          return commitsSince(cwd, since ?? undefined)
        }
        return { text: `no such tool: ${call.tool}`, is_error: true }
      },
    }))
  },
})
