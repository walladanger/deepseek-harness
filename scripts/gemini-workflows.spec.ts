/** Regression coverage for Gemini workflow authorization, checkout, and CLI preparation. */
import { spawnSync } from 'node:child_process'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { runInNewContext } from 'node:vm'
import { load } from 'js-yaml'
import { describe, expect, it } from 'vitest'

interface Step {
  name: string
  if?: string
  uses?: string
  run?: string
  env?: Record<string, string>
  with?: Record<string, string | number>
  'continue-on-error'?: boolean
}

interface Workflow {
  concurrency: { group: string; 'cancel-in-progress': boolean }
  jobs: Record<string, { if: string; steps: Step[] }>
}

const repository = 'example/harness'
const token = 'synthetic-workflow-token'
const workflows = [
  { name: 'gemini-invoke', job: 'assist' },
  { name: 'gemini-pr-review', job: 'review' },
].map(({ name, job }) => {
  const workflow = load(readFileSync(new URL(`../.github/workflows/${name}.yml`, import.meta.url), 'utf8')) as Workflow
  return { name, workflow, job: workflow.jobs[job]! }
})
const invoke = workflows[0]!
const review = workflows[1]!

function step(steps: Step[], name: string): Step {
  const result = steps.find(candidate => candidate.name === name)
  if (!result) throw new Error(`Missing workflow step: ${name}`)
  return result
}

function commentEvent(pullRequest = true, body = '@gemini-cli /review', association = 'OWNER', number = 42) {
  return {
    event_name: 'issue_comment', repository, token,
    event: {
      repository: { default_branch: 'main' },
      pull_request: {},
      issue: { number, ...(pullRequest ? { pull_request: { url: 'https://example.test/pr/42' } } : {}) },
      comment: { body, author_association: association },
    },
  }
}

function pullRequestEvent(headRepository = repository, number = 42) {
  return {
    event_name: 'pull_request', repository, token,
    ref: `refs/pull/${number}/merge`,
    event: {
      repository: { default_branch: 'main' },
      pull_request: { number, head: { repo: { full_name: headRepository } } },
      issue: {}, comment: {},
    },
  }
}

// These fixtures use the shared boolean/string subset of Actions and JavaScript;
// this evaluator does not model general Actions coercion or status functions.
function evaluate(expression: string, github: object): unknown {
  return runInNewContext(expression.trim().replace(/^\$\{\{|\}\}$/g, ''), {
    github,
    contains: (value: string, search: string) => value.toLowerCase().includes(search.toLowerCase()),
  }, { timeout: 1000 })
}

function render(template: string, github: object): string {
  return template.replace(/\$\{\{([\s\S]*?)\}\}/g, (_, expression: string) => String(evaluate(expression, github)))
}

function runShell(script: string, cwd?: string, variables: Record<string, string> = {}) {
  const result = spawnSync('bash', ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', script], {
    cwd, encoding: 'utf8', timeout: 10_000,
    env: { PATH: process.env.PATH, ...variables },
  })
  expect(result.error).toBeUndefined()
  expect(result.signal).toBeNull()
  return result
}

describe('Gemini review authorization', () => {
  it('accepts same-repository PR events without requiring a comment', () => {
    expect(evaluate(review.job.if, pullRequestEvent())).toBe(true)
  })

  it.each(['other/harness', 'example/other', ''])('rejects PR events from head repository %j', (head) => {
    expect(evaluate(review.job.if, pullRequestEvent(head))).toBe(false)
  })

  it.each(['OWNER', 'COLLABORATOR', 'MEMBER'])('accepts review comments from %s', (association) => {
    expect(evaluate(review.job.if, commentEvent(true, '@gemini-cli /review', association))).toBe(true)
  })

  it.each(['CONTRIBUTOR', 'FIRST_TIME_CONTRIBUTOR', 'FIRST_TIMER', 'NONE', 'MANNEQUIN', ''])(
    'rejects review comments from %j', (association) => {
      expect(evaluate(review.job.if, commentEvent(true, '@gemini-cli /review', association))).toBe(false)
    },
  )

  it.each(['', '@gemini-cli please explain', '/review', 'please review this PR'])(
    'rejects incomplete review request %j', (body) => {
      expect(evaluate(review.job.if, commentEvent(true, body))).toBe(false)
    },
  )

  it('rejects review requests on issues', () => {
    expect(evaluate(review.job.if, commentEvent(false))).toBe(false)
  })
})

describe('Gemini event routing', () => {
  it.each([1, 42, 123456])('routes PR #%i exclusively to the PR input', (number) => {
    const inputs = step(invoke.job.steps, 'Run Gemini CLI').with!
    const event = commentEvent(true, '@gemini-cli fix this', 'OWNER', number)
    expect(evaluate(String(inputs.github_pr_number), event)).toBe(number)
    expect(evaluate(String(inputs.github_issue_number), event)).toBe('')
  })

  it.each([1, 42, 123456])('routes issue #%i exclusively to the issue input', (number) => {
    const inputs = step(invoke.job.steps, 'Run Gemini CLI').with!
    const event = commentEvent(false, '@gemini-cli fix this', 'OWNER', number)
    expect(evaluate(String(inputs.github_pr_number), event)).toBe('')
    expect(evaluate(String(inputs.github_issue_number), event)).toBe(number)
  })

  it.each([pullRequestEvent(), commentEvent()])('reviews the event PR number on $event_name', (event) => {
    const inputs = step(review.job.steps, 'Run Gemini CLI review').with!
    expect(evaluate(String(inputs.github_pr_number), event)).toBe(42)
    expect(render(String(inputs.prompt), event)).toContain('pull request #42')
  })

  it('cancels stale reviews for the same PR across both event types', () => {
    expect(review.workflow.concurrency['cancel-in-progress']).toBe(true)
    const group = review.workflow.concurrency.group
    expect(render(group, pullRequestEvent())).toBe(render(group, commentEvent()))
    expect(render(group, pullRequestEvent())).not.toBe(render(group, pullRequestEvent(repository, 43)))
  })

  it('starts review checkout from the default branch even without a merge ref', () => {
    const checkout = step(review.job.steps, 'Checkout')
    expect(evaluate(String(checkout.with?.ref), pullRequestEvent())).toBe('main')
    expect(evaluate(String(checkout.with?.ref), commentEvent())).toBe('main')
  })

  it('checks out the named PR branch for either review trigger', () => {
    const checkout = step(review.job.steps, 'Check out the PR head')
    for (const event of [pullRequestEvent(), commentEvent()]) {
      expect(render(checkout.run!, event)).toBe('gh pr checkout 42')
    }
  })

  it('skips PR verification and checkout for issue invocations', () => {
    const verification = step(invoke.job.steps, "Verify the PR head is this repository's own branch")
    const checkout = step(invoke.job.steps, 'Check out the PR branch')
    for (const candidate of [verification, checkout]) {
      expect(Boolean(evaluate(candidate.if!, commentEvent(false)))).toBe(false)
      expect(Boolean(evaluate(candidate.if!, commentEvent()))).toBe(true)
    }
    expect(render(checkout.run!, commentEvent())).toBe('gh pr checkout 42')
  })

  it('verifies review comment heads at runtime', () => {
    const verification = step(review.job.steps, "Verify the PR head is this repository's own branch")
    expect(evaluate(verification.if!, commentEvent())).toBe(true)
    expect(evaluate(verification.if!, pullRequestEvent())).toBe(false)
  })

  it('instructs issue fixes to create a branch and PR', () => {
    const prompt = render(String(step(invoke.job.steps, 'Run Gemini CLI').with?.prompt), commentEvent(false))
      .replace(/\s+/g, ' ')
    expect(prompt).toContain('issue #42')
    expect(prompt).toContain('never commit or push to the current branch')
    expect(prompt).toContain('create a new branch, commit the change there, and open a pull request')
  })
})

describe.each(workflows)('$name CLI preparation', ({ name, job }) => {
  const steps = job.steps
  const verification = step(steps, "Verify the PR head is this repository's own branch")
  const checkout = step(steps, name === 'gemini-invoke' ? 'Check out the PR branch' : 'Check out the PR head')
  const cleanup = step(steps, 'Remove untrusted workspace configuration')
  const authentication = step(steps, 'Authenticate GitHub CLI')
  const cli = step(steps, name === 'gemini-invoke' ? 'Run Gemini CLI' : 'Run Gemini CLI review')

  it('verifies the head before checkout and prepares the workspace before Gemini', () => {
    const ordered = [step(steps, 'Checkout'), verification, checkout, cleanup, authentication, cli]
    for (let index = 1; index < ordered.length; index++) {
      expect(steps.indexOf(ordered[index]!)).toBeGreaterThan(steps.indexOf(ordered[index - 1]!))
    }
    expect(step(steps, 'Checkout').with?.['fetch-depth']).toBe(0)
    for (const candidate of [verification, checkout, cleanup, authentication]) {
      expect(candidate['continue-on-error']).toBeUndefined()
    }
    expect(checkout.env?.GH_TOKEN).toBe('${{ github.token }}')
    expect(verification.env?.GH_TOKEN).toBe('${{ github.token }}')
  })

  it('explicitly trusts the prepared workspace in the Gemini action environment', () => {
    expect(cli.uses).toMatch(/^google-github-actions\/run-gemini-cli@/)
    expect(cli.env?.GEMINI_CLI_TRUST_WORKSPACE).toBe('true')
    expect(cli.with?.gemini_api_key).toBe('${{ secrets.GEMINI_CLI_KEY }}')
  })

  // Both workflows target Ubuntu; native Windows does not provide their Bash shell.
  describe.skipIf(process.platform === 'win32')('Ubuntu shell steps', () => {
    it.each([
      { head: repository, status: 0 },
      { head: 'outsider/harness', status: 1 },
      { head: 'example/other', status: 1 },
      { head: '', status: 1 },
    ])('accepts only the exact head repository: $head', ({ head, status }) => {
      const script = `
        gh() {
          printf '%s\\n' "$*" >&2
          printf '%s\\n' "$TEST_HEAD_REPOSITORY"
        }
        ${render(verification.run!, commentEvent())}
      `
      const result = runShell(script, undefined, { TEST_HEAD_REPOSITORY: head })
      expect(result.stderr.trim()).toBe('pr view 42 --json headRepository --jq .headRepository.nameWithOwner')
      expect(result.status).toBe(status)
      if (status !== 0) expect(result.stdout).toContain('::error::')
    })

    it('fails closed when the head lookup fails', () => {
      const result = runShell(`gh() { return 17; }\n${render(verification.run!, commentEvent())}`)
      expect(result.status).toBe(17)
    })

    it('removes Gemini and dotenv configuration while preserving agent changes', () => {
      const directory = mkdtempSync(join(tmpdir(), 'gemini-workflow-'))
      try {
        mkdirSync(join(directory, '.gemini'))
        mkdirSync(join(directory, '.agents'))
        writeFileSync(join(directory, '.gemini', 'settings.json'), '{}')
        writeFileSync(join(directory, '.env'), 'SYNTHETIC=true\n')
        writeFileSync(join(directory, '.agents', 'note.md'), 'in-flight agent change\n')
        writeFileSync(join(directory, 'source.ts'), 'export const value = 1\n')
        for (let attempt = 0; attempt < 2; attempt++) {
          const result = runShell(cleanup.run!, directory)
          expect(result.status).toBe(0)
          expect(existsSync(join(directory, '.gemini'))).toBe(false)
          expect(existsSync(join(directory, '.env'))).toBe(false)
          expect(readFileSync(join(directory, '.agents', 'note.md'), 'utf8')).toBe('in-flight agent change\n')
          expect(readFileSync(join(directory, 'source.ts'), 'utf8')).toBe('export const value = 1\n')
        }
      } finally {
        rmSync(directory, { recursive: true, force: true })
      }
    })

    it('authenticates gh through stdin before configuring git credentials', () => {
      const result = runShell(`
        gh() {
          printf '%s\\n' "$*"
          if [ "$*" = 'auth login --with-token' ]; then
            IFS= read -r supplied_token
            [ "$supplied_token" = "$TEST_TOKEN" ] || return 19
          fi
        }
        ${render(authentication.run!, commentEvent())}
      `, undefined, { TEST_TOKEN: token })
      expect(result.status).toBe(0)
      expect(result.stdout.trim().split('\n')).toEqual(['auth login --with-token', 'auth setup-git'])
    })

    it('stops before git credential setup when gh login fails', () => {
      const result = runShell(`
        gh() {
          printf '%s\\n' "$*"
          if [ "$*" = 'auth login --with-token' ]; then
            cat >/dev/null
            return 23
          fi
        }
        ${render(authentication.run!, commentEvent())}
      `)
      expect(result.status).toBe(23)
      expect(result.stdout.trim()).toBe('auth login --with-token')
    })
  })
})
