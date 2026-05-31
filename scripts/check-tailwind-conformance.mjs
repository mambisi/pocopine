#!/usr/bin/env node
// Tailwind conformance guard for Pine Stylekit (RFC 092 D3).
//
// When a new Tailwind version ships, this script answers two questions:
//
//   1. Which utility / variant FAMILIES did Tailwind add (or remove) since
//      the version Stylekit was last checked against? Added families are
//      the actionable signal — "Tailwind 4.4 introduced `mask-luminance`,
//      Stylekit should consider it." This is the CI gate.
//
//   2. Which families does Stylekit COVER today? Coverage is
//      machine-derived: per Tailwind `test('family')` block, Stylekit
//      covers the family iff it resolves ≥1 of that block's own positive
//      candidates. Gaps become an unchecked checklist.
//
// The gap report is no longer a committed Markdown doc — instead this
// script renders it into a **living GitHub issue** (label
// `tailwind-coverage`, never closed) and updates it idempotently on each
// CI run. `--sub-issues` additionally opens one tracking issue per gap.
//
// It also refreshes the committed corpus fixtures so the grammar +
// coverage oracles track the pinned Tailwind version.
//
// Usage:
//   node scripts/check-tailwind-conformance.mjs                 # latest from npm
//   node scripts/check-tailwind-conformance.mjs --version 4.3.0
//   node scripts/check-tailwind-conformance.mjs --local tmp/tailwind
//   node scripts/check-tailwind-conformance.mjs --update        # rewrite fixtures
//   node scripts/check-tailwind-conformance.mjs --issue         # post/update living issue (needs gh)
//   node scripts/check-tailwind-conformance.mjs --issue --sub-issues
//   node scripts/check-tailwind-conformance.mjs --check         # CI: exit 1 on new families
//   node scripts/check-tailwind-conformance.mjs --no-cargo      # skip the resolve probe
//
// Node ≥18 (uses global fetch). `--issue` needs the `gh` CLI + an
// `issues: write` token. No other external dependencies.

import { readFileSync, writeFileSync, existsSync, mkdtempSync } from 'node:fs'
import { resolve, dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { tmpdir } from 'node:os'
import { execFileSync } from 'node:child_process'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const FIX = resolve(ROOT, 'crates/pocopine-stylekit/tests/fixtures')
const FILES = ['utilities.test.ts', 'variants.test.ts', 'candidate.test.ts']

const args = process.argv.slice(2)
const flag = (name) => args.includes(name)
const opt = (name) => {
  const i = args.indexOf(name)
  return i >= 0 ? args[i + 1] : undefined
}

const UPDATE = flag('--update')
const CHECK = flag('--check')
const NO_CARGO = flag('--no-cargo')
const LOCAL = opt('--local')
const ISSUE = flag('--issue') // post/update the living coverage issue
const PRINT_BODY = flag('--print-body') // print the issue body to stdout
const SUB_ISSUES = flag('--sub-issues') // also open one issue per gap family
const LABEL = opt('--label') ?? 'tailwind-coverage'
const GAP_LABEL = opt('--gap-label') ?? 'tailwind-gap'

// ── source acquisition ──────────────────────────────────────────────

async function latestVersion() {
  const res = await fetch('https://registry.npmjs.org/tailwindcss/latest')
  if (!res.ok) throw new Error(`npm registry: ${res.status}`)
  return (await res.json()).version
}

async function loadSources(version) {
  const sources = {}
  if (LOCAL) {
    const base = resolve(ROOT, LOCAL, 'packages/tailwindcss/src')
    for (const f of FILES) {
      const p = join(base, f)
      if (existsSync(p)) sources[f] = readFileSync(p, 'utf8')
    }
    if (!sources['utilities.test.ts']) {
      throw new Error(`no utilities.test.ts under ${base}`)
    }
    return sources
  }
  const tag = `v${version}`
  for (const f of FILES) {
    const url = `https://raw.githubusercontent.com/tailwindlabs/tailwindcss/${tag}/packages/tailwindcss/src/${f}`
    const res = await fetch(url)
    if (res.ok) sources[f] = await res.text()
    else if (f !== 'candidate.test.ts') {
      throw new Error(`fetch ${url}: ${res.status}`)
    }
  }
  return sources
}

// ── extraction ──────────────────────────────────────────────────────

// `test('NAME', …)` at column 0 — one per utility/variant family.
const FAMILY_RE = /^test\(\s*'([^']+)'/gm

function familiesOf(text, kind) {
  const out = new Set()
  for (const m of text.matchAll(FAMILY_RE)) out.add(`${kind}\t${m[1]}`)
  return out
}

// Scan the `[ … ]` array starting at `text[open]` (which must be `[`),
// returning [arrayBody, indexAfterClosingBracket]. Bracket depth is
// tracked while skipping string literals, so candidate values that
// themselves contain `[`/`]` (e.g. `'mask-[url(/m.png)]'`) don't close
// the array early — the failure mode a naive `\[…\]` regex hits.
function scanArray(text, open) {
  let depth = 0
  let i = open
  for (; i < text.length; i++) {
    const c = text[i]
    if (c === '"' || c === "'" || c === '`') {
      // Skip to the matching, unescaped quote.
      i++
      while (i < text.length && text[i] !== c) {
        if (text[i] === '\\') i++
        i++
      }
      continue
    }
    if (c === '[') depth++
    else if (c === ']') {
      depth--
      if (depth === 0) return [text.slice(open + 1, i), i + 1]
    }
  }
  return [text.slice(open + 1), text.length]
}

const STR_RE = /'([^'\n]+)'|"([^"\n]+)"/g

// Candidates are passed to one of Tailwind's test drivers:
//   run([CANDIDATES])  ·  compile([CANDIDATES])  ·  compileCss(css`…`, [CANDIDATES][, opts])
// i.e. the array is NOT always the first argument (`compileCss` takes a
// CSS template first). For each call, scan its full paren scope, pull
// every bracket-balanced `[…]` array inside it (skipping string/template
// literals, so a css template's own brackets don't leak in), and capture
// the assertion that follows: `.toEqual('')` → emits nothing; else the
// inline-snapshot CSS so we can see which candidates were *actually*
// emitted.
const CALL_RE = /\b(?:run|compile|compileCss)\s*\(/g

function* eachCall(text) {
  let m
  while ((m = CALL_RE.exec(text))) {
    const openParen = text.indexOf('(', m.index)
    const end = scanParen(text, openParen)
    CALL_RE.lastIndex = end
    const body = text.slice(openParen + 1, end - 1)
    const tokens = new Set()
    let i = 0
    while (i < body.length) {
      const c = body[i]
      if (c === '"' || c === "'" || c === '`') {
        i++
        while (i < body.length && body[i] !== c) {
          if (body[i] === '\\') i++
          i++
        }
        i++
        continue
      }
      if (c === '[') {
        const [arr, after] = scanArray(body, i)
        for (const t of arr.matchAll(STR_RE)) {
          const tok = t[1] ?? t[2]
          // `example*` is Tailwind's test-only `@utility example-*`, not a
          // real utility — exclude it from the conformance corpus.
          if (tok && !/[\s{}<>$]/.test(tok) && !/^-?example($|[-/])/.test(tok)) {
            tokens.add(tok)
          }
        }
        i = after
        continue
      }
      i++
    }
    // The assertion follows the call, possibly past a trailing comma +
    // the closing `)` of `expect(await run(…),)` — so allow `,`, `)`, ws.
    const after = text.slice(end, end + 80)
    const isNeg = /^[\s),]*\.toEqual\(\s*(''|"")\s*\)/.test(after)
    // Capture the expected CSS snapshot (if any) for the emitted-filter.
    let snapshot = null
    if (/^[\s),]*\.toMatchInlineSnapshot\(/.test(after)) {
      const bt = text.indexOf('`', end)
      if (bt >= 0) {
        let j = bt + 1
        while (j < text.length && text[j] !== '`') {
          if (text[j] === '\\') j++
          j++
        }
        snapshot = text.slice(bt + 1, j)
      }
    }
    yield { tokens, isNeg, snapshot }
  }
}

// Is class `tok` actually present as a selector in the (de-escaped) CSS
// snapshot? Tailwind escapes `[`, `:`, `/`, … with `\` in selectors;
// stripping the backslashes lets us match the raw candidate. A trailing
// boundary stops `m-4` matching inside `.m-40`.
function emittedIn(tok, deEscaped) {
  const re = new RegExp(
    '\\.' + tok.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + '(?=[\\s.:,){>+~]|$)',
  )
  return re.test(deEscaped)
}

function candidatesOf(sources) {
  const positive = new Set()
  const negative = new Set()
  for (const text of Object.values(sources)) {
    for (const { tokens, isNeg, snapshot } of eachCall(text)) {
      if (isNeg) {
        for (const tok of tokens) negative.add(tok)
        continue
      }
      if (snapshot != null) {
        // Snapshot present → a candidate is positive only if Tailwind
        // actually emitted a rule for it. The rest are invalid forms
        // Tailwind packs into the same array (and emits nothing for).
        const deEscaped = snapshot.replace(/\\/g, '')
        for (const tok of tokens) {
          if (emittedIn(tok, deEscaped)) positive.add(tok)
          else negative.add(tok)
        }
      } else {
        // No snapshot (e.g. `.toMatch`, custom assertions) — can't
        // verify emission; keep the candidates as positive.
        for (const tok of tokens) positive.add(tok)
      }
    }
  }
  // A candidate that is positive anywhere counts as positive.
  for (const p of positive) negative.delete(p)
  return { positive, negative }
}

// Scan a balanced `( … )` starting at `text[open]` (`(`), respecting
// strings / template literals. Returns the index just past the close.
function scanParen(text, open) {
  let depth = 0
  for (let i = open; i < text.length; i++) {
    const c = text[i]
    if (c === '"' || c === "'" || c === '`') {
      i++
      while (i < text.length && text[i] !== c) {
        if (text[i] === '\\') i++
        i++
      }
      continue
    }
    if (c === '(') depth++
    else if (c === ')') {
      depth--
      if (depth === 0) return i + 1
    }
  }
  return text.length
}

// Per-family positive candidates: for each `test('NAME', …)` block,
// collect the positive candidates declared inside it. Lets us mark a
// family covered iff Stylekit resolves at least one of its own examples
// — machine-derived coverage, no hand-maintained list.
function familyCandidates(sources) {
  const map = new Map() // "kind\tname" -> Set(positive candidates)
  const byFile = [
    ['utilities.test.ts', 'utility'],
    ['variants.test.ts', 'variant'],
  ]
  for (const [file, kind] of byFile) {
    const text = sources[file]
    if (!text) continue
    const testRe = /^test\(\s*'([^']+)'/gm
    let m
    while ((m = testRe.exec(text))) {
      const name = m[1]
      const open = text.indexOf('(', m.index)
      const end = scanParen(text, open)
      const block = text.slice(open, end)
      const { positive } = candidatesOf({ block })
      map.set(`${kind}\t${name}`, positive)
    }
  }
  return map
}

// ── fixtures ────────────────────────────────────────────────────────

function readLines(name) {
  const p = join(FIX, name)
  if (!existsSync(p)) return []
  return readFileSync(p, 'utf8')
    .split('\n')
    .filter((l) => l && !l.startsWith('#'))
}

function writeList(name, header, items) {
  const sorted = [...items].sort()
  writeFileSync(join(FIX, name), `${header}\n${sorted.join('\n')}\n`)
  return sorted.length
}

// ── main ────────────────────────────────────────────────────────────

const version = opt('--version') ?? (LOCAL ? 'local' : await latestVersion())
const sources = await loadSources(version)

const families = new Set([
  ...familiesOf(sources['utilities.test.ts'] ?? '', 'utility'),
  ...familiesOf(sources['variants.test.ts'] ?? '', 'variant'),
])
const { positive, negative } = candidatesOf(sources)

const baselineVersion = (readLines('tailwind-version.txt')[0] ?? '0').trim()
const baselineFamilies = new Set(readLines('tailwind-families.txt'))

const added = [...families].filter((f) => !baselineFamilies.has(f)).sort()
const removed = [...baselineFamilies].filter((f) => !families.has(f)).sort()
const versionChanged = version !== 'local' && version !== baselineVersion

if (UPDATE) {
  writeList(
    'tailwind-candidates.txt',
    `# Tailwind class-candidate corpus (grammar oracle, RFC 092 D3).\n# version ${version} — generated by scripts/check-tailwind-conformance.mjs.`,
    new Set([...positive, ...negative]),
  )
  writeList(
    'tailwind-positive.txt',
    `# Tailwind POSITIVE candidate corpus (resolve-rate oracle).\n# version ${version} — candidates Tailwind emits CSS for. Generated; do not edit.`,
    positive,
  )
  writeList(
    'tailwind-families.txt',
    `# Tailwind family inventory (test() names).\n# version ${version} — generated by scripts/check-tailwind-conformance.mjs.\n# Format: <kind>\\t<family>`,
    families,
  )
  writeFileSync(join(FIX, 'tailwind-version.txt'), `${version}\n`)
}

// Probe Stylekit for resolution over the *fresh* positive corpus (so
// coverage reflects the version under check, not the pinned fixture).
// The Rust oracle dumps the unresolved set; a family is COVERED iff
// Stylekit resolves ≥1 of that family's own positive candidates.
let unresolved = new Set()
let resolveRate = null
let probed = false
if (!NO_CARGO) {
  const dir = mkdtempSync(join(tmpdir(), 'sk-conf-'))
  const corpusPath = join(dir, 'positive.txt')
  const reportPath = join(dir, 'unresolved.txt')
  writeFileSync(corpusPath, [...positive].sort().join('\n') + '\n')
  try {
    execFileSync(
      'cargo',
      ['test', '-p', 'pocopine-stylekit', '--test', 'tailwind_conformance', '--', '--nocapture'],
      {
        cwd: ROOT,
        env: { ...process.env, STYLEKIT_CORPUS: corpusPath, STYLEKIT_CONFORMANCE_REPORT: reportPath },
        stdio: ['ignore', 'pipe', 'pipe'],
      },
    )
  } catch {
    // Probe mode doesn't assert the floor, but guard anyway.
  }
  if (existsSync(reportPath)) {
    const text = readFileSync(reportPath, 'utf8')
    resolveRate = text.split('\n')[0].replace(/^#\s*resolved\s*/, '')
    unresolved = new Set(
      text.split('\n').filter((l) => l && !l.startsWith('#')),
    )
    probed = true
  }
}

// Per-family coverage from the probe.
const famPositives = familyCandidates(sources)
const covered = new Set() // "kind\tname" Stylekit resolves
const gaps = new Set() // real families with no resolving candidate
const sorters = new Set() // families with no class candidates (ordering tests)
for (const fam of families) {
  const cands = famPositives.get(fam)
  if (!cands || cands.size === 0) {
    sorters.add(fam)
    continue
  }
  if (probed && [...cands].some((c) => !unresolved.has(c))) covered.add(fam)
  else if (probed) gaps.add(fam)
}

const split = (set) => ({
  utility: [...set].filter((f) => f.startsWith('utility\t')).map((f) => f.split('\t')[1]).sort(),
  variant: [...set].filter((f) => f.startsWith('variant\t')).map((f) => f.split('\t')[1]).sort(),
})
const cov = split(covered)
const gap = split(gaps)
const realCount = families.size - sorters.size
const coveredCount = covered.size
const pct = realCount ? Math.round((coveredCount / realCount) * 100) : 0

// ── living-issue body ───────────────────────────────────────────────

const checklist = (covList, gapList) =>
  [
    ...gapList.map((n) => `- [ ] \`${n}\``),
    ...covList.map((n) => `- [x] \`${n}\``),
  ].join('\n') || '_none_'

function issueBody() {
  const b = []
  b.push('> **Living document — do not close.** Regenerated by')
  b.push('> [`scripts/check-tailwind-conformance.mjs`](scripts/check-tailwind-conformance.mjs)')
  b.push('> on each `tailwind-conformance` CI run. Hand edits are overwritten;')
  b.push('> close gaps in code, not here.')
  b.push('')
  b.push(`**Tailwind ${version}** · Pine Stylekit family coverage: **${coveredCount}/${realCount} (${pct}%)**`)
  b.push('')
  b.push('Coverage is machine-derived: a family is ✅ iff Stylekit resolves at')
  b.push("least one of that family's own Tailwind test candidates. Stylekit is a")
  b.push('typed subset (RFC 092 D3) — unchecked items are expressible today via')
  b.push('the custom-CSS passthrough until natively supported.')
  b.push('')
  if (added.length || removed.length) {
    b.push('### ⚠ Upstream inventory changed vs pinned baseline')
    b.push(`Baseline \`${baselineVersion}\` → checking \`${version}\`.`)
    for (const f of added) b.push(`- ➕ \`${f.split('\t')[1]}\` (${f.split('\t')[0]}) — new in Tailwind`)
    for (const f of removed) b.push(`- ➖ \`${f.split('\t')[1]}\` (${f.split('\t')[0]}) — removed upstream`)
    b.push('')
  }
  b.push(`### Utility gaps (${gap.utility.length}) · covered ${cov.utility.length}`)
  b.push('')
  b.push('<details open><summary>checklist</summary>')
  b.push('')
  b.push(checklist(cov.utility, gap.utility))
  b.push('')
  b.push('</details>')
  b.push('')
  b.push(`### Variant gaps (${gap.variant.length}) · covered ${cov.variant.length}`)
  b.push('')
  b.push('<details><summary>checklist</summary>')
  b.push('')
  b.push(checklist(cov.variant, gap.variant))
  b.push('')
  b.push('</details>')
  b.push('')
  b.push(`<sub>${sorters.size} non-utility "tests" (sorter/ordering) excluded. Last run: ${version}.</sub>`)
  return b.join('\n')
}

// ── console / step-summary report ───────────────────────────────────

const report = [
  `# Tailwind conformance — v${version}`,
  '',
  `- Baseline: \`${baselineVersion}\`${versionChanged ? ` → **\`${version}\`** (new)` : ' (current)'}`,
  `- Family coverage: **${coveredCount}/${realCount} (${pct}%)** — ${gap.utility.length} utility + ${gap.variant.length} variant gaps`,
  // Internal regression tripwire only — NOT a coverage metric (the
  // corpus is saturated with arbitrary/negative forms a subset omits).
  resolveRate ? `- (candidate resolve tripwire: ${resolveRate})` : null,
  added.length ? `- ⚠ ${added.length} new upstream famil${added.length === 1 ? 'y' : 'ies'}` : null,
  removed.length ? `- ${removed.length} removed upstream` : null,
  !added.length && !removed.length ? '- Inventory matches baseline ✅' : null,
]
  .filter(Boolean)
  .join('\n')
console.log(report)
if (process.env.GITHUB_STEP_SUMMARY) {
  writeFileSync(process.env.GITHUB_STEP_SUMMARY, report + '\n', { flag: 'a' })
}

if (PRINT_BODY) {
  console.log('\n' + '─'.repeat(60) + '\n')
  console.log(issueBody())
}

// ── living issue (idempotent; never closed) ─────────────────────────

function gh(args, input) {
  return execFileSync('gh', args, {
    cwd: ROOT,
    input,
    encoding: 'utf8',
    stdio: ['pipe', 'pipe', 'inherit'],
  }).trim()
}

if (ISSUE) {
  if (!probed) {
    console.error('✖ --issue needs the resolve probe; do not combine with --no-cargo.')
    process.exit(1)
  }
  // Ensure labels exist (idempotent).
  for (const [name, color, desc] of [
    [LABEL, '1d76db', 'Tailwind ↔ Stylekit coverage (living)'],
    [GAP_LABEL, 'fbca04', 'A specific Tailwind family Stylekit does not yet cover'],
  ]) {
    try {
      gh(['label', 'create', name, '--color', color, '--description', desc, '--force'])
    } catch {
      /* label may exist / perms */
    }
  }

  const title = 'Tailwind ↔ Stylekit coverage (living)'
  const body = issueBody()
  const existing = JSON.parse(
    gh(['issue', 'list', '--label', LABEL, '--state', 'all', '--limit', '1', '--json', 'number,state']) || '[]',
  )
  if (existing.length) {
    const { number, state } = existing[0]
    if (state !== 'OPEN') gh(['issue', 'reopen', String(number)])
    gh(['issue', 'edit', String(number), '--title', title, '--body-file', '-'], body)
    console.log(`updated living issue #${number}`)
  } else {
    const url = gh(['issue', 'create', '--label', LABEL, '--title', title, '--body-file', '-'], body)
    console.log(`created living issue: ${url}`)
  }

  // Optional: one tracking sub-issue per gap family (idempotent by title).
  if (SUB_ISSUES) {
    const open = JSON.parse(
      gh(['issue', 'list', '--label', GAP_LABEL, '--state', 'open', '--limit', '500', '--json', 'title']) || '[]',
    ).map((i) => i.title)
    for (const kind of ['utility', 'variant']) {
      for (const name of gap[kind]) {
        const t = `[tailwind-gap] ${kind}: ${name}`
        if (open.includes(t)) continue
        const sb = `Pine Stylekit does not yet cover the Tailwind **${kind}** family \`${name}\` (Tailwind ${version}).\n\nTracked by the living coverage issue. Close by implementing it in \`crates/pocopine-stylekit/src/registry.rs\` (the next conformance run will tick its checklist box).`
        gh(['issue', 'create', '--label', GAP_LABEL, '--title', t, '--body-file', '-'], sb)
        console.log(`opened sub-issue: ${t}`)
      }
    }
  }
}

// CI gate: a new Tailwind version that adds families we don't track is a
// failure to investigate (then bump the baseline with --update).
if (CHECK && added.length) {
  console.error(`\n✖ ${added.length} new Tailwind famil${added.length === 1 ? 'y' : 'ies'} not in baseline.`)
  console.error('  Review coverage, refresh the baseline, regenerate the issue:')
  console.error(`  node scripts/check-tailwind-conformance.mjs --version ${version} --update --issue`)
  process.exit(1)
}
