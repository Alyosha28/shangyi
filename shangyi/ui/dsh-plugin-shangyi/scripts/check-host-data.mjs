#!/usr/bin/env node
// 宿主数据面的独立自检（不依赖 DSH 运行时、不联网）。
//
// 跑法：node scripts/check-host-data.mjs
//
// 检查四件事：
//   1) 知识库索引：篇数、字段齐全、group 来源是 front-matter 的 kind；
//   2) 单篇正文：正常 slug 能读，越界 slug（.. / 绝对路径 / 非 .md）必须被拒；
//   3) 案例摘要：phase / status 字段齐全，且与**另写一遍**的独立推导逐条一致；
//   4) 卡片投影向后兼容：meta / matrix / solve / selection / failures 都在。
//
// 为什么第 3 条要另写一遍推导：同一段代码自己验自己等于没验。这里用「只看原始事件类型」
// 的第二套实现与 inferProgress 对拍，判据不同写法、同一真相源。
import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'
import os from 'node:os'

const here = path.dirname(new URL(import.meta.url).pathname)
const mod = await import(path.join(here, '..', 'lib', 'index.js'))

const KB_EXPECTED = 35
let tempFixtureHome = null
let activeHome = process.env.SHANGYI_HOME || path.join(os.homedir(), '.shangyi')
if (mod.listCases(activeHome).filter((c) => c.size > 0).length === 0) {
  tempFixtureHome = fs.mkdtempSync(path.join(os.tmpdir(), 'shangyi-host-data-fixture-'))
  const caseDir = path.join(tempFixtureHome, 'cases', '2026-11-12', 'case-20261112-scenario-fixture')
  fs.mkdirSync(caseDir, { recursive: true })
  fs.writeFileSync(path.join(caseDir, 'header.json'), JSON.stringify({
    case_id: 'case-20261112-scenario-fixture',
    mode: 'scenario',
    playbook_id: 'pricing_response',
    created_at: '2026-11-12T10:00:00Z',
    schema_version: 1
  }))
  const fixtureEvents = [
    { seq: 1, event: { type: 'task_scope_selected', scope: 'consultation', reason: '完整推演', at: '2026-11-12T10:00:01Z' } },
    { seq: 2, event: { type: 'phase_entered', from: 'intake', to: 'intel', playbook: 'pricing_response', reason: 'ok', at: '2026-11-12T10:00:02Z' } },
    { seq: 3, event: { type: 'checkpoint_opened', id: 'cp1', phase: 'intel', items: ['同行价格样本已核对'], at: '2026-11-12T10:00:03Z' } },
    { seq: 4, event: { type: 'turn_started', at: '2026-11-12T10:00:04Z' } },
    { seq: 5, event: { type: 'turn_ended', at: '2026-11-12T10:00:05Z' } }
  ]
  fs.writeFileSync(path.join(caseDir, 'case.jsonl'), fixtureEvents.map((x) => JSON.stringify(x)).join('\n') + '\n')
  process.env.SHANGYI_HOME = tempFixtureHome
  activeHome = tempFixtureHome
}
const CASES_HOME = activeHome
const checks = []
function check(name, fn) { checks.push({ name, fn }) }

// ── 1. 知识库索引 ───────────────────────────────────────────────────────────
check('kb 索引：篇数 = 35', () => {
  const r = mod.listKb()
  assert.equal(r.ok, true, r.error)
  assert.equal(r.docs.length, KB_EXPECTED, `实际 ${r.docs.length} 篇`)
  for (const d of r.docs) {
    assert.ok(d.slug && typeof d.slug === 'string', 'slug 缺失')
    assert.ok(!d.slug.endsWith('.md'), `slug 不该带 .md：${d.slug}`)
    assert.ok(d.title && typeof d.title === 'string', `title 缺失：${d.slug}`)
    assert.ok(d.path.startsWith('shangyi/kb/'), `path 不在仓库内：${d.path}`)
    assert.ok(d.group === 'theory' || d.group === 'method' || d.group === 'concept', `group 非法：${d.slug} → ${d.group}`)
  }
  const dist = {}
  for (const d of r.docs) dist[d.group] = (dist[d.group] || 0) + 1
  return `35 篇；group 分布 ${JSON.stringify(dist)}；前 3 条 ${r.docs.slice(0, 3).map((d) => d.slug).join(' / ')}`
})

// ── 2. 单篇正文 ─────────────────────────────────────────────────────────────
check('kb 正文：正常 slug', () => {
  const r = mod.readKbDoc('gamespec-build')
  assert.equal(r.ok, true, r.error)
  assert.ok(r.markdown.includes('# 为什么这一步不能交给模型自由发挥'), '正文内容不符')
  assert.ok(r.title.length > 0, 'title 空')
  return `slug=${r.slug} title=${r.title} group=${r.group} ${r.markdown.length} 字节`
})

check('kb 正文：越界 slug 全部被拒', () => {
  const bad = ['../package.json', '/etc/passwd', 'a/../../x', 'foo.txt', '', '..', 'theory/../../secret.md', '\0abc']
  const seen = []
  for (const s of bad) {
    const r = mod.readKbDoc(s)
    assert.equal(r.ok, false, `未拒绝：${JSON.stringify(s)}`)
    assert.ok(r.status === 400 || r.status === 404, `状态码异常：${JSON.stringify(s)} → ${r.status}`)
    assert.ok(!('markdown' in r), `拒绝时不该返回正文：${JSON.stringify(s)}`)
    seen.push(`${JSON.stringify(s)}=${r.status}`)
  }
  return seen.join(' ')
})

// ── 3. 案例摘要 + 独立对拍 ──────────────────────────────────────────────────
// 第二套推导：只用事件顺序，不共享 inferProgress 的代码路径。
function independentProgress(events) {
  let phase = null
  const openIds = []
  for (const e of events) {
    if (e.type === 'phase_entered' && e.to) phase = e.to
    if (e.type === 'checkpoint_opened' && e.id) {
      const i = openIds.indexOf(e.id)
      if (i >= 0) openIds.splice(i, 1)
      openIds.push(e.id)
    }
    if (e.type === 'checkpoint_answered' && e.id) {
      const first = Array.isArray(e.answers) && e.answers.length ? String(e.answers[0] || '') : ''
      if (!first.startsWith('追问：')) {
        const i = openIds.indexOf(e.id)
        if (i >= 0) openIds.splice(i, 1)
      }
    }
  }
  const last = events.length ? events[events.length - 1] : null
  let openTurn = false
  for (const e of events) {
    if (e.type === 'turn_started') openTurn = true
    if (e.type === 'turn_ended') openTurn = false
  }
  let lastTool = null
  for (const e of events) if (e.type === 'tool_call_finished') lastTool = e
  const failed = Boolean(lastTool && (lastTool.outcome || {}).status === 'failed')

  let status = null
  if (phase === 'archived') status = 'done'
  else if (openIds.length > 0) status = 'waiting'
  else if (failed) status = 'stuck'
  else if (openTurn) status = 'running'
  return { phase, status, pending: openIds[openIds.length - 1] || null, lastEvent: last && last.type }
}

check('案例摘要：字段齐全 + 与独立推导逐条一致', () => {
  const cases = mod.summarizeCases(30)
  assert.ok(cases.length > 0, '没有案例可验')
  // 独立取一遍案例目录（按 mtime 倒序、跳过空壳），与 summarizeCases 同覆盖同顺序
  const dirs = mod.listCases(CASES_HOME).filter((c) => c.size > 0).slice(0, cases.length)
  assert.equal(dirs.length, cases.length, `覆盖不一致：${dirs.length} vs ${cases.length}`)

  const dist = {}
  const rows = []
  let compared = 0
  for (let i = 0; i < cases.length; i++) {
    const c = cases[i]
    const d = dirs[i]
    assert.equal(c.caseId, d.caseId, `第 ${i} 条案例对不上：${c.caseId} vs ${d.caseId}`)
    assert.ok('phase' in c, `缺 phase：${c.caseId}`)
    assert.ok('status' in c, `缺 status：${c.caseId}`)
    assert.ok([null, 'running', 'waiting', 'done', 'stuck'].includes(c.status), `status 非法：${c.caseId} → ${c.status}`)

    const events = mod.readCase(d.log).events
    const want = independentProgress(events)
    assert.equal(c.phase, want.phase, `phase 不一致：${c.caseId} ${c.phase} vs ${want.phase}`)
    assert.equal(c.status, want.status, `status 不一致：${c.caseId} ${c.status} vs ${want.status}`)
    assert.equal(c.pendingCheckpoint, want.pending, `pending 不一致：${c.caseId}`)
    assert.equal(c.lastEvent, want.lastEvent, `lastEvent 不一致：${c.caseId}`)
    compared++
    dist[c.status] = (dist[c.status] || 0) + 1
    if (c.status === 'stuck' || c.status === 'waiting') rows.push(`${c.caseId.slice(0, 34)} status=${c.status} phase=${c.phase} pending=${c.pendingCheckpoint} lastToolOk=${c.lastToolOk}`)
  }
  return `${compared}/${cases.length} 条逐条对拍通过；status 分布 ${JSON.stringify(dist)}` +
    (rows.length ? `\n  waiting/stuck 明细：\n  ` + rows.join('\n  ') : '')
})

// ── 4. 卡片投影向后兼容 ─────────────────────────────────────────────────────
check('cards 投影：字段未删（向后兼容）', () => {
  let r
  try { r = mod.projectCase('latest-cards') } catch (e) {
    // 没有案例时允许抛错，但形状检查用一个已知案例重试
    const first = mod.summarizeCases(1)[0]
    assert.ok(first, '连一个案例都没有，无法验证 cards')
    r = mod.projectCase(first.caseId)
  }
  for (const k of ['source', 'meta', 'matrix', 'solve', 'selection', 'failures']) {
    assert.ok(k in r, `cards 缺字段：${k}`)
  }
  for (const k of ['case_id', 'mode', 'playbook', 'events', 'turns', 'tokens', 'phases', 'checkpoints', 'tools']) {
    assert.ok(k in r.meta, `meta 缺字段：${k}`)
  }
  return `case=${r.meta.case_id} events=${r.meta.events} phases=${r.meta.phases.length} matrix=${r.matrix ? '有' : '无'} solve=${r.solve ? '有' : '无'}`
})

check('apply/name 导出未变', () => {
  assert.equal(mod.name, 'dsh-plugin-shangyi')
  assert.equal(typeof mod.apply, 'function')
  for (const fn of ['listCases', 'readCase', 'projectCase', 'summarizeCases', 'latestCaseWithCards', 'inferProgress', 'listKb', 'readKbDoc', 'answerCheckpoint']) {
    assert.equal(typeof mod[fn], 'function', `缺少导出：${fn}`)
  }
  return '8 个既有导出 + inferProgress/listKb/readKbDoc/answerCheckpoint'
})

// ── 5. 路由注册（用假 connection，不需要 DSH 运行时）────────────────────────
check('路由注册：4 条 GET + 1 条 POST + 处理器返回码', async () => {
  const routes = []
  const labels = []
  const fakeConn = {
    fetch: {
      register(cfg) { routes.push(cfg); return () => {} }
    }
  }
  const fakeCtx = {
    inject(deps, cb) {
      assert.deepEqual(deps, ['connection'])
      cb({
        get: () => fakeConn,
        effect(fn, label) { labels.push(label); const d = fn(); assert.equal(typeof d, 'function', `${label} 的清理函数缺失`); return d },
        logger: { warn: () => {} }
      })
    }
  }
  mod.apply(fakeCtx)

  const paths = routes.map((r) => r.path).sort()
  assert.deepEqual(paths, ['/api/shangyi/cards', '/api/shangyi/cases', '/api/shangyi/checkpoint', '/api/shangyi/kb', '/api/shangyi/kb/doc'])
  for (const r of routes) {
    const wantMethods = r.path === '/api/shangyi/checkpoint' ? ['POST'] : ['GET']
    assert.deepEqual(r.methods, wantMethods, `${r.path} 方法不符：${JSON.stringify(r.methods)}`)
  }
  assert.equal(routes.length, 5, `注册了 ${routes.length} 条路由`)
  assert.equal(labels.length, 5, `effect 清理项 ${labels.length} 条`)

  const byPath = Object.fromEntries(routes.map((r) => [r.path, r]))
  const call = async (p, qs = '', init = undefined) => {
    const res = await byPath[p].fetch(new Request(`http://127.0.0.1${p}${qs}`, init))
    return { status: res.status, body: await res.json(), cache: res.headers.get('cache-control') }
  }

  const kb = await call('/api/shangyi/kb')
  assert.equal(kb.status, 200); assert.equal(kb.body.ok, true); assert.equal(kb.body.docs.length, KB_EXPECTED)
  assert.equal(kb.cache, 'no-store')

  const doc = await call('/api/shangyi/kb/doc', '?slug=equilibrium-selection')
  assert.equal(doc.status, 200); assert.equal(doc.body.ok, true); assert.ok(doc.body.markdown.includes('---'))

  const evil = await call('/api/shangyi/kb/doc', '?slug=..%2F..%2Fetc%2Fpasswd')
  assert.equal(evil.status, 400); assert.equal(evil.body.ok, false)

  const missing = await call('/api/shangyi/kb/doc', '?slug=no-such-doc')
  assert.equal(missing.status, 404)

  const cases = await call('/api/shangyi/cases')
  assert.equal(cases.status, 200); assert.ok(Array.isArray(cases.body.cases)); assert.ok(cases.body.cases.length > 0)
  assert.ok('phase' in cases.body.cases[0] && 'status' in cases.body.cases[0])

  const cards = await call('/api/shangyi/cards')
  assert.equal(cards.status, 200); assert.ok(cards.body.ok); assert.ok('meta' in cards.body && 'matrix' in cards.body)

  // POST /api/shangyi/checkpoint 校验与隔离写入测试
  const badCt = await call('/api/shangyi/checkpoint', '', {
    method: 'POST',
    headers: { 'content-type': 'text/plain' },
    body: '{}'
  })
  assert.equal(badCt.status, 415); assert.equal(badCt.body.ok, false)

  const badOrigin = await call('/api/shangyi/checkpoint', '', {
    method: 'POST',
    headers: { 'content-type': 'application/json', origin: 'https://evil.example' },
    body: '{}'
  })
  assert.equal(badOrigin.status, 403); assert.equal(badOrigin.body.ok, false)

  const badJson = await call('/api/shangyi/checkpoint', '', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: '{bad-json'
  })
  assert.equal(badJson.status, 400); assert.equal(badJson.body.ok, false)

  const missingCase = await call('/api/shangyi/checkpoint', '', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ caseId: 'no-such-case-999999', decision: 'approve' })
  })
  assert.equal(missingCase.status, 404); assert.equal(missingCase.body.ok, false)

  const cpHome = fs.mkdtempSync(path.join(os.tmpdir(), 'shangyi-cp-route-'))
  const prevHome = process.env.SHANGYI_HOME
  try {
    const cDir = path.join(cpHome, 'cases', '2026-11-12', 'case-cp-route-test')
    fs.mkdirSync(cDir, { recursive: true })
    fs.writeFileSync(path.join(cDir, 'case.jsonl'), [
      JSON.stringify({ seq: 1, event: { type: 'phase_entered', from: 'intake', to: 'intel', at: '2026-11-12T10:00:00Z' } }),
      JSON.stringify({ seq: 2, event: { type: 'checkpoint_opened', id: 'cp1', phase: 'intel', items: ['测试关口'], at: '2026-11-12T10:00:01Z' } })
    ].join('\n') + '\n')
    process.env.SHANGYI_HOME = cpHome
    const cpOk = await call('/api/shangyi/checkpoint', '', {
      method: 'POST',
      headers: { 'content-type': 'application/json', origin: 'http://127.0.0.1' },
      body: JSON.stringify({ caseId: 'case-cp-route-test', checkpointId: 'cp1', decision: 'approve', note: '通过' })
    })
    assert.equal(cpOk.status, 200); assert.equal(cpOk.body.ok, true); assert.equal(cpOk.body.checkpointId, 'cp1')
    const cpDup = await call('/api/shangyi/checkpoint', '', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ caseId: 'case-cp-route-test', checkpointId: 'cp1', decision: 'approve' })
    })
    assert.equal(cpDup.status, 409); assert.equal(cpDup.body.ok, false)
  } finally {
    if (prevHome === undefined) delete process.env.SHANGYI_HOME
    else process.env.SHANGYI_HOME = prevHome
    fs.rmSync(cpHome, { recursive: true, force: true })
  }

  return `GET×4 + POST×1；kb=200(${kb.body.docs.length}) doc=200 evil=400 missing=404 cases=200(${cases.body.cases.length}) cards=200 checkpoint=415/403/400/404/200/409`
})

// ── 执行 + 输出 ──────────────────────────────────────────────────────────────
const results = []
try {
  for (const { name, fn } of checks) {
    try { results.push({ name, ok: true, detail: await fn() }) }
    catch (e) { results.push({ name, ok: false, detail: String((e && e.message) || e) }) }
  }
} finally {
  if (tempFixtureHome) {
    delete process.env.SHANGYI_HOME
    fs.rmSync(tempFixtureHome, { recursive: true, force: true })
  }
}

let failed = 0
for (const r of results) {
  console.log(`${r.ok ? 'PASS' : 'FAIL'}  ${r.name}`)
  if (r.detail) console.log(`      ${String(r.detail).replace(/\n/g, '\n      ')}`)
  if (!r.ok) failed++
}
console.log(`\n${results.length - failed} passed / ${failed} failed`)
process.exit(failed ? 1 : 0)
