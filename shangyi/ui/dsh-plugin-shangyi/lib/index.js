// 商奕领域卡片 / 工作台的 Node 半（宿主数据面）。
//
// 职责（本版）：
//   1) 占住 cordis 插件行（浏览器 bundle 的宿主）；
//   2) 注册**只读**、经 DSH 浏览器鉴权的路由：
//      - `/api/shangyi/cards`  案例卡片投影（既有，保持向后兼容）
//      - `/api/shangyi/cases`  案例列表摘要（本版新增 phase / status）
//      - `/api/shangyi/kb`     知识库索引（仓库内 shangyi/kb）
//      - `/api/shangyi/kb/doc` 单篇正文
//
// 为什么读 case log 而不是读 DSH 会话事件：
//   - case log 是商奕的**唯一真相源**（工具入参、结局、checkpoint、阶段推进都在里面），
//     且已经落盘、可离线对拍；读它不依赖 DSH 内部会话 API 的稳定性。
//   - 本版不做 per-session 绑定：`?case=latest|<case_id>` 显式选择案例，
//     避免把「最近一次」当成「当前会话」（那会把别人的案例显示成你的）。
//
// 口径纪律（商奕铁律 1 的延伸）：本文件只**读**确定性产物，不推断数值。
//   - 阶段名直接取日志里的 `phase_entered.to`；
//   - 状态由下面 inferProgress() 的三条可核判据推出，判据写在该函数注释里；
//   - 判据不成立时返回 null，不猜。
import fs from 'node:fs'
import path from 'node:path'
import os from 'node:os'
import { fileURLToPath } from 'node:url'

export const name = 'dsh-plugin-shangyi'

function defaultHome() {
  return process.env.SHANGYI_HOME || path.join(os.homedir(), '.shangyi')
}

// 仓库根：<repo>/shangyi/ui/dsh-plugin-shangyi/lib/index.js → 上溯 4 层。
// 知识库只在仓库里，不在 ~/.shangyi，所以单独解析。
// 解析顺序：显式环境变量 → 由本文件位置推导 → 进程 cwd（验证实例的 cwd 就是仓库根）。
function findRepoRoot() {
  const candidates = []
  if (process.env.SHANGYI_ROOT) candidates.push(process.env.SHANGYI_ROOT)
  try {
    const here = path.dirname(fileURLToPath(import.meta.url))
    candidates.push(path.resolve(here, '..', '..', '..', '..'))
  } catch { /* 非 ESM 加载时跳过 */ }
  candidates.push(process.cwd())
  for (const c of candidates) {
    try {
      if (fs.statSync(path.join(c, 'shangyi', 'kb')).isDirectory()) return c
    } catch { /* 继续找 */ }
  }
  return null
}

const REPO_ROOT = findRepoRoot()
const KB_DIR = REPO_ROOT ? path.join(REPO_ROOT, 'shangyi', 'kb') : null

/** 列出全部案例目录（cases/<date>/<case_id>），新的在前；跳过 AppleDouble `._*` 与隐藏项。 */
export function listCases(home = defaultHome()) {
  const root = path.join(home, 'cases')
  const out = []
  let days = []
  try { days = fs.readdirSync(root) } catch { return out }
  for (const day of days) {
    if (!day || day.startsWith('.')) continue
    const dayDir = path.join(root, day)
    let ids = []
    try { ids = fs.readdirSync(dayDir) } catch { continue }
    for (const id of ids) {
      if (!id || id.startsWith('.')) continue
      const dir = path.join(dayDir, id)
      const log = path.join(dir, 'case.jsonl')
      let stat
      try { stat = fs.statSync(log) } catch { continue }
      out.push({ caseId: id, dir, log, size: stat.size, mtime: stat.mtimeMs })
    }
  }
  out.sort((a, b) => b.mtime - a.mtime)
  return out
}

function resolveCaseTarget(caseRef = 'latest', home = defaultHome()) {
  const cases = listCases(home)
  if (!cases.length) throw new Error(`在 ${path.join(home, 'cases')} 下没有找到任何案例日志`)
  if (!caseRef || caseRef === 'latest' || caseRef === 'active') {
    try {
      const activeRaw = JSON.parse(fs.readFileSync(path.join(home, 'cases', 'active.json'), 'utf8'))
      const activeId = activeRaw && (activeRaw.case_id || activeRaw.caseId)
      if (activeId) {
        const hit = cases.find((c) => c.caseId === activeId && c.size > 0)
        if (hit && caseRef === 'active') return hit
        // latest 优先按 mtime 取最新非空案例；若 active 就是最新之一也自然命中
      }
    } catch { /* active.json 可选 */ }
    return cases.find((c) => c.size > 0) || cases[0]
  }
  const target = cases.find((c) => c.caseId === caseRef || c.dir.endsWith('/' + caseRef))
  if (!target) throw new Error(`找不到案例 \`${caseRef}\``)
  return target
}

/** 读 case.jsonl；坏行不计入（但会记在 meta.warnings 里）。同时返回最大 seq。 */
export function readCase(logPath) {
  const events = []
  let bad = 0
  let maxSeq = 0
  for (const line of fs.readFileSync(logPath, 'utf8').split('\n')) {
    const t = line.trim()
    if (!t) continue
    try {
      const rec = JSON.parse(t)
      if (rec && typeof rec.seq === 'number' && rec.seq > maxSeq) maxSeq = rec.seq
      if (rec && rec.event) events.push(rec.event)
    } catch { bad++ }
  }
  return { events, badLines: bad, maxSeq }
}

/** 支付值表达式 → 展示文本。引擎的铁律：数值以参数引用表示，不在前端编造终值。 */
function exprText(v) {
  if (v == null) return '?'
  if (typeof v === 'number') return String(v)
  if (typeof v === 'object') {
    if (v.kind === 'literal') return String(v.value)
    if (v.kind === 'ref') return `⟨${v.value}⟩`
    if (v.value !== undefined) return String(v.value)
  }
  return String(v)
}

function isPlainObject(v) {
  return Boolean(v) && typeof v === 'object' && !Array.isArray(v)
}

function jsonValuesEqual(a, b) {
  if (a === b) return true
  if (Array.isArray(a) && Array.isArray(b)) {
    return a.length === b.length && a.every((item, i) => jsonValuesEqual(item, b[i]))
  }
  if (isPlainObject(a) && isPlainObject(b)) {
    const keysA = Object.keys(a)
    const keysB = Object.keys(b)
    return (
      keysA.length === keysB.length &&
      keysA.every((k) => Object.prototype.hasOwnProperty.call(b, k) && jsonValuesEqual(a[k], b[k]))
    )
  }
  return false
}

function deepMergeJson(base, patch) {
  if (isPlainObject(base) && isPlainObject(patch)) {
    const hasOwn = Object.prototype.hasOwnProperty
    const kindChanged = hasOwn.call(base, 'kind') && hasOwn.call(patch, 'kind') && !jsonValuesEqual(base.kind, patch.kind)
    const formChanged = hasOwn.call(base, 'form') && hasOwn.call(patch, 'form') && !jsonValuesEqual(base.form, patch.form)
    if (kindChanged || formChanged) {
      return deepMergeJson(null, patch)
    }
    const out = { ...base }
    for (const [k, v] of Object.entries(patch)) {
      out[k] = hasOwn.call(out, k) ? deepMergeJson(out[k], v) : deepMergeJson(null, v)
    }
    return out
  }
  if (Array.isArray(patch)) return patch.map((x) => deepMergeJson(null, x))
  return patch
}

/** 把一次 gamespec_build 的入参解析成 spec（兼容模型把参数塞进 arguments 字符串或 spec 键的形状）。 */
function unwrapSpec(argsRaw) {
  if (!argsRaw) return null
  let v
  try { v = JSON.parse(argsRaw) } catch { return null }
  // 实测存在 {"arguments":"{...}"} 的双层形状
  for (let i = 0; i < 3 && isPlainObject(v) && typeof v.arguments === 'string'; i++) {
    try { v = JSON.parse(v.arguments) } catch { break }
  }
  if (isPlainObject(v) && isPlainObject(v.spec) && !v.players && !v.params) {
    const outerMerge = v.merge
    v = typeof outerMerge === 'boolean' && v.spec.merge === undefined
      ? { ...v.spec, merge: outerMerge }
      : v.spec
  }
  return isPlainObject(v) ? v : null
}

/** 合并所有 gamespec_build 入参（支持 merge:true 增量构建与完整覆盖），得到模型最终想建的博弈骨架。 */
function mergeSpecs(specs) {
  const out = { players: null, strategies: null, payoffs: null, params: null, game_id: null }
  for (const s of specs) {
    if (!isPlainObject(s)) continue
    const inc = s.merge === true
    if (Array.isArray(s.players) && s.players.length) out.players = s.players
    if (isPlainObject(s.strategies) && Object.keys(s.strategies).length) {
      out.strategies = inc && out.strategies
        ? deepMergeJson(out.strategies, s.strategies)
        : deepMergeJson(null, s.strategies)
    }
    if (isPlainObject(s.payoffs)) {
      out.payoffs = inc && out.payoffs
        ? deepMergeJson(out.payoffs, s.payoffs)
        : deepMergeJson(null, s.payoffs)
    }
    if (isPlainObject(s.params)) {
      out.params = inc && out.params
        ? deepMergeJson(out.params, s.params)
        : deepMergeJson(null, s.params)
    }
    if (typeof s.game_id === 'string' && s.game_id) out.game_id = s.game_id
  }
  return out
}

function matrixCard(spec) {
  if (!spec || !spec.players || !spec.strategies) return null
  const players = spec.players.map((p) => ({
    id: String(p.id ?? p.name ?? '?'),
    name: String(p.name ?? p.id ?? '?')
  }))
  const strategies = spec.strategies
  const rowP = players[0]
  const colP = players[1] || null
  if (!rowP || !colP) return null
  const rows = strategies[rowP.id] || strategies[rowP.name] || []
  const cols = strategies[colP.id] || strategies[colP.name] || []
  if (!rows.length || !cols.length) return null

  // payoffs：支持 {form,values:{cells:{...}}} / {cells:{...}} / 其他 → 取 cells 子对象
  let cells = null
  let form = null
  const pay = spec.payoffs || {}
  if (pay && typeof pay === 'object') {
    form = pay.form ?? (pay.values && pay.values.form) ?? null
    const values = pay.values && typeof pay.values === 'object' ? pay.values : pay
    if (values.cells && typeof values.cells === 'object') cells = values.cells
  }

  const grid = []
  for (const r of rows) {
    const line = []
    for (const c of cols) {
      const key = `${r},${c}`
      const cell = cells ? cells[key] : null
      line.push({
        key,
        cells: cell && typeof cell === 'object'
          ? players.map((p) => ({ player: p.name, expr: exprText(cell[p.id]) }))
          : []
      })
    }
    grid.push({ strategy: r, cells: line })
  }
  return {
    game_id: spec.game_id || null,
    players,
    rows,
    cols,
    form,
    grid,
    hasPayoffCells: Boolean(cells && Object.keys(cells).length),
    params: spec.params ? Object.keys(spec.params).length : 0
  }
}

function parseJsonSafe(raw) {
  if (raw == null) return null
  if (typeof raw === 'object') return raw
  if (typeof raw !== 'string' || !raw.trim()) return null
  try { return JSON.parse(raw) } catch { return null }
}

function summarizeParamSource(src) {
  if (!src || typeof src !== 'object') return { sourceKind: 'unknown', sourceDetail: '' }
  const kind = String(src.kind || 'unknown')
  if (kind === 'user_confirmed') {
    return {
      sourceKind: kind,
      checkpoint: src.checkpoint || null,
      sourceDetail: src.checkpoint ? `关口 ${src.checkpoint}` : '用户当面确认'
    }
  }
  if (kind === 'confirmed_assumption') {
    const parts = [src.checkpoint ? `关口 ${src.checkpoint}` : '', src.basis || ''].filter(Boolean)
    return {
      sourceKind: kind,
      checkpoint: src.checkpoint || null,
      basis: src.basis || null,
      sourceDetail: parts.join(' · ')
    }
  }
  if (kind === 'public_data') {
    return { sourceKind: kind, provenance: src.provenance || null, sourceDetail: src.provenance || '公开数据' }
  }
  if (kind === 'industry_prior') {
    return { sourceKind: kind, priorRef: src.prior_ref || null, sourceDetail: src.prior_ref || '行业先验（非本案实证）' }
  }
  if (kind === 'assumed') {
    return { sourceKind: kind, note: src.note || null, sourceDetail: src.note || '未确认假设（禁止入解）' }
  }
  if (kind === 'derived') {
    return { sourceKind: kind, note: src.note || null, sourceDetail: src.note || '引擎派生' }
  }
  return { sourceKind: kind, sourceDetail: '' }
}

function isRollbackRewind(fromPhase, toPhase, dueTo) {
  const from = typeof fromPhase === 'string' ? fromPhase : ''
  const to = typeof toPhase === 'string' ? toPhase : ''
  if (!from || from === to) return false
  const d = typeof dueTo === 'string' ? dueTo.trim() : ''
  const dl = d.toLowerCase()
  return (
    d.includes('（用户确认：') ||
    d.includes('模型提议') ||
    d.includes('用户同意') ||
    dl.startsWith('rollback') ||
    dl.startsWith('user_rewind') ||
    dl.startsWith('request_rewind')
  )
}

function rewindProjectionState(st, e) {
  const toPhase = e && e.to
  const rank = PHASE_ORDER_INDEX[toPhase]
  if (typeof rank !== 'number') return
  const clearTarget = !isRollbackRewind(e.from, toPhase, e.due_to)
  const cutoff = clearTarget ? rank : rank + 1
  if (cutoff <= 2) st.specBuilds = []
  if (cutoff <= 3) st.solve = null
  if (cutoff <= 4) st.fuse = null
  if (cutoff <= 5) st.consequence = null
  if (cutoff <= 6) st.discovery = null
  if (cutoff <= 7) {
    st.recommend = null
    st.conditional = null
    st.scopedResult = null
  }
  if (cutoff <= 8) st.reportModel = null
  if (rank === 0 && clearTarget) {
    st.scope = null
    st.scopeReason = null
    st.costs.clear()
    st.constraints = []
    st.constraintCheck = null
  }
  st.failures = []
  for (const cp of st.checkpointTimeline) {
    if (cp.status === 'pending' || cp.status === 'answered') {
      const cpRank = PHASE_ORDER_INDEX[cp.phase]
      if ((typeof cpRank === 'number' && cpRank >= cutoff) || (toPhase === 'intake' && clearTarget)) {
        cp.status = 'rewound'
      }
    }
  }
}

function resetProjectionForFollowup(st, e = null) {
  const wasConditional =
    st.scope === 'conditional' ||
    Boolean(st.conditional) ||
    Boolean(st.scopedResult && st.scopedResult.scope === 'conditional') ||
    Boolean(e && (e.prior_scope === 'conditional' || e.scope === 'conditional' || e.task_scope === 'conditional'))
  st.scope = null
  st.scopeReason = null
  st.scopedResult = null
  st.conditional = null
  st.failures = []
  if (wasConditional) {
    st.specBuilds = []
    st.solve = null
    st.fuse = null
    st.consequence = null
    st.discovery = null
    st.recommend = null
    st.reportModel = null
    st.constraintCheck = null
    for (const cp of st.checkpointTimeline) {
      if (cp.status === 'pending' || cp.status === 'answered') cp.status = 'rewound'
    }
  }
}

/** 案例日志 → 卡片数据。纯函数（除读盘外），便于对拍。 */
export function projectCase(caseRef = 'latest', home = defaultHome()) {
  const target = resolveCaseTarget(caseRef, home)

  const { events, badLines } = readCase(target.log)
  const started = new Map()
  const st = {
    scope: null,
    scopeReason: null,
    specBuilds: [],
    solve: null,
    fuse: null,
    consequence: null,
    discovery: null,
    recommend: null,
    reportModel: null,
    scopedResult: null,
    conditional: null,
    costs: new Map(),
    constraints: [],
    constraintCheck: null,
    poiSurveys: [],
    researchRecords: [],
    webSources: new Map(),
    kbQueries: [],
    checkpointTimeline: [],
    toolsLog: [],
    failures: [],
    exploratoryCalls: 0,
    phases: [],
    checkpointsAnswered: [],
    callsCount: 0,
    tokens: 0,
    turns: 0
  }

  for (const e of events) {
    if (!e || !e.type) continue
    if (e.type === 'phase_entered' && e.to) st.phases.push(e.to)
    else if (e.type === 'phase_rewound' && e.to) {
      rewindProjectionState(st, e)
      st.phases.push(`回退→${e.to}`)
    } else if (e.type === 'followup_task_started') {
      resetProjectionForFollowup(st, e)
    } else if (e.type === 'task_scope_selected' && e.scope) {
      st.scope = e.scope
      st.scopeReason = e.reason || null
    } else if (e.type === 'scoped_task_completed' && e.result) {
      if (e.result.scope) st.scope = e.result.scope
      st.scopedResult = e.result
      if (e.result.conditional) st.conditional = e.result.conditional
      if (Array.isArray(e.result.source_urls)) {
        for (const u of e.result.source_urls) {
          if (u && String(u).trim() && !st.webSources.has(u)) {
            st.webSources.set(u, { url: String(u), title: null, snippet: null, tool: 'scoped_task_completed' })
          }
        }
      }
    } else if (e.type === 'checkpoint_opened' && e.id) {
      for (const prev of st.checkpointTimeline) {
        if (prev.id === e.id && prev.status === 'pending') prev.status = 'superseded'
      }
      st.checkpointTimeline.push({
        id: String(e.id),
        phase: e.phase || null,
        items: Array.isArray(e.items) ? e.items.map(String) : [],
        status: 'pending',
        answers: [],
        actor: null,
        openedAt: e.at || null,
        answeredAt: null
      })
    } else if (e.type === 'checkpoint_answered' && e.id) {
      st.checkpointsAnswered.push(e.id)
      const first = Array.isArray(e.answers) && e.answers.length ? String(e.answers[0] || '') : ''
      const isFollowup = first.startsWith('追问：')
      for (let i = st.checkpointTimeline.length - 1; i >= 0; i--) {
        const cp = st.checkpointTimeline[i]
        if (cp.id === e.id && cp.status === 'pending') {
          if (Array.isArray(e.answers)) cp.answers.push(...e.answers.map(String))
          cp.actor = e.user_actor || null
          if (!isFollowup) {
            cp.status = 'answered'
            cp.answeredAt = e.at || null
          }
          break
        }
      }
    } else if (e.type === 'token_usage') {
      st.tokens += (e.input_tokens || 0) + (e.output_tokens || 0)
    } else if (e.type === 'turn_started') {
      st.turns += 1
    } else if (e.type === 'tool_call_started') {
      started.set(e.call_id, e)
      if (e.name === 'sandbox_exec') st.exploratoryCalls += 1
    } else if (e.type === 'tool_call_finished') {
      st.callsCount += 1
      const s = started.get(e.call_id) || {}
      const name = s.name || '?'
      const outcome = e.outcome || {}
      const status = outcome.status || 'unknown'
      const exploratory = name === 'sandbox_exec'

      st.toolsLog.push({
        callId: e.call_id || null,
        name,
        status,
        exploratory,
        at: e.at || s.at || null,
        detail: status === 'ok'
          ? (exploratory ? '探索性沙箱试算（已隔离排除出正式结论）' : '')
          : String(outcome.error ?? outcome.detail ?? outcome.output ?? '').slice(0, 300)
      })

      if (status !== 'ok') {
        st.failures.push({
          tool: name,
          status,
          detail: String(outcome.error ?? outcome.detail ?? outcome.output ?? '').slice(0, 600)
        })
        continue
      }

      // 铁律 1：sandbox_exec 属于 TrustLevel::Exploratory，绝不写入任何权威业务/博弈状态
      if (exploratory) continue

      const argsObj = unwrapSpec(s.arguments || '')
      const parsedOut = parseJsonSafe(outcome.output)
      if (name === 'gamespec_build') {
        if (argsObj) st.specBuilds.push(argsObj)
      } else if (name === 'solve_equilibria' && parsedOut) {
        st.solve = parsedOut
      } else if (name === 'select_fuse' && parsedOut) {
        st.fuse = parsedOut
      } else if (name === 'consequence_predict' && parsedOut) {
        st.consequence = parsedOut
      } else if (name === 'discover_satisfice' && parsedOut) {
        st.discovery = parsedOut
      } else if (name === 'recommend_build' && parsedOut) {
        st.recommend = parsedOut
      } else if (name === 'report_render' && parsedOut && parsedOut.report) {
        st.reportModel = parsedOut.report
      } else if (name === 'business_cost' && parsedOut) {
        const key = String(parsedOut.scenario_id || parsedOut.scenario || e.call_id || st.costs.size)
        st.costs.set(key, parsedOut)
      } else if (name === 'constraint_register' && parsedOut) {
        st.constraints.push(parsedOut)
      } else if (name === 'constraint_check' && parsedOut) {
        st.constraintCheck = parsedOut
      } else if (name === 'select_task_scope' && parsedOut) {
        if (parsedOut.scope) st.scope = parsedOut.scope
        if (parsedOut.reason) st.scopeReason = parsedOut.reason
      } else if (name === 'conditional_analysis') {
        st.scope = (parsedOut && parsedOut.scope) || 'conditional'
        if (parsedOut) {
          st.scopedResult = parsedOut
          if (parsedOut.conditional) st.conditional = parsedOut.conditional
        }
      } else if (name === 'poi_survey' && parsedOut) {
        st.poiSurveys.push(parsedOut)
      } else if (name === 'research_record' && parsedOut) {
        st.researchRecords.push(parsedOut)
      } else if (name === 'web_search' && parsedOut && Array.isArray(parsedOut.results)) {
        for (const item of parsedOut.results) {
          if (item && item.url) {
            st.webSources.set(item.url, {
              url: String(item.url),
              title: item.title ? String(item.title) : null,
              snippet: item.snippet ? String(item.snippet) : null,
              tool: 'web_search'
            })
          }
        }
      } else if (name === 'web_fetch') {
        const u = (parsedOut && parsedOut.url) || (argsObj && argsObj.url) || null
        if (u) {
          st.webSources.set(u, {
            url: String(u),
            title: (parsedOut && parsedOut.title) ? String(parsedOut.title) : null,
            snippet: null,
            tool: 'web_fetch'
          })
        }
      } else if (name === 'kb_retrieve' && argsObj && argsObj.query) {
        st.kbQueries.push({ query: String(argsObj.query), at: e.at || s.at || null })
      }
    }
  }

  const spec = mergeSpecs(st.specBuilds)
  const progress = inferProgress(events)

  let header = null
  try { header = JSON.parse(fs.readFileSync(path.join(target.dir, 'header.json'), 'utf8')) } catch { /* 老案例可能没有 */ }

  // 提取核心参数与四类证据来源
  const paramCounts = {
    user_confirmed: 0,
    confirmed_assumption: 0,
    public_data: 0,
    industry_prior: 0,
    assumed: 0,
    derived: 0,
    unknown: 0
  }
  const paramsList = []
  const gaps = []
  if (spec.params && typeof spec.params === 'object') {
    for (const [k, p] of Object.entries(spec.params)) {
      if (!p || typeof p !== 'object') continue
      const srcInfo = summarizeParamSource(p.source)
      if (Object.prototype.hasOwnProperty.call(paramCounts, srcInfo.sourceKind)) {
        paramCounts[srcInfo.sourceKind] += 1
      } else {
        paramCounts.unknown += 1
      }
      if (srcInfo.sourceKind === 'assumed' || srcInfo.sourceKind === 'unknown') {
        gaps.push(`参数「${p.name || k}」来源为 ${srcInfo.sourceKind}，未经确认不得静默进入正式求解`)
      }
      paramsList.push({
        key: k,
        name: p.name || k,
        valueText: exprText(p.value),
        unit: p.unit || null,
        ...srcInfo
      })
    }
  }

  const costScenarios = Array.from(st.costs.values())
  const missingCostItems = []
  for (const sc of costScenarios) {
    if (Array.isArray(sc.items)) {
      for (const it of sc.items) {
        if (it && (it.normalized_amount === null || it.normalized_amount === undefined)) {
          const reason = it.missing_amount_reason || '未取得具体金额'
          missingCostItems.push({
            scenario: sc.scenario || sc.scenario_id || '当前方案',
            item: it.name || '未命名费用',
            reason
          })
          gaps.push(`${sc.scenario || '方案'}·${it.name || '费用项'}：未取得（${reason}）`)
        }
      }
    }
  }
  if (st.scopedResult && Array.isArray(st.scopedResult.missing)) {
    for (const m of st.scopedResult.missing) if (m) gaps.push(String(m))
  }
  if (st.conditional && Array.isArray(st.conditional.limitations)) {
    for (const l of st.conditional.limitations) if (l) gaps.push(String(l))
  }

  const bossSection = st.reportModel && Array.isArray(st.reportModel.sections)
    ? st.reportModel.sections.find((sec) => sec && sec.layer === 'boss_page')
    : null
  const bossHeadline = bossSection && Array.isArray(bossSection.paragraphs) && bossSection.paragraphs.length
    ? bossSection.paragraphs[0]
    : null

  const hasEngineResult = Boolean(
    st.solve ||
    st.fuse ||
    st.consequence ||
    st.discovery ||
    st.recommend ||
    st.reportModel ||
    costScenarios.length ||
    st.scopedResult
  )

  return {
    source: { path: target.log, caseId: target.caseId, mtime: new Date(target.mtime).toISOString(), badLines },
    meta: {
      case_id: header?.case_id || target.caseId,
      mode: header?.mode || null,
      playbook: header?.playbook_id || null,
      created_at: header?.created_at || null,
      events: events.length,
      turns: st.turns,
      tokens: st.tokens,
      scope: st.scope,
      phases: st.phases,
      checkpoints: st.checkpointsAnswered,
      tools: st.callsCount
    },
    pendingCheckpoint: progress.pendingCheckpoint,
    pendingCheckpointDetail: progress.pendingCheckpointDetail,
    matrix: matrixCard(spec),
    solve: st.solve
      ? {
          completeness: st.solve.completeness || null,
          route: st.solve.route || null,
          equilibria: st.solve.equilibria || [],
          refinement: st.solve.refinement || null
        }
      : null,
    selection: st.fuse
      ? {
          entries: st.fuse.entries || [],
          confidence: st.fuse.confidence || null,
          divergences: st.fuse.divergences || []
        }
      : null,
    failures: st.failures.slice(-5),
    scopeDetail: {
      scope: st.scope,
      reason: st.scopeReason,
      scopedResult: st.scopedResult,
      conditional: st.conditional
    },
    costs: {
      scenarios: costScenarios,
      constraints: st.constraints,
      constraintCheck: st.constraintCheck,
      missingItems: missingCostItems
    },
    assumptions: {
      players: Array.isArray(spec.players)
        ? spec.players.map((p) => ({
            id: String(p.id ?? p.name ?? '?'),
            name: String(p.name ?? p.id ?? '?'),
            vnRole: p.vn_role || null,
            relation: p.relation_to_game || null
          }))
        : [],
      params: paramsList,
      counts: paramCounts,
      gaps
    },
    evidence: {
      checkpoints: st.checkpointTimeline,
      sources: Array.from(st.webSources.values()),
      poiSurveys: st.poiSurveys,
      researchRecords: st.researchRecords,
      kbQueries: st.kbQueries,
      tools: st.toolsLog.slice(-40),
      exploratoryCalls: st.exploratoryCalls
    },
    conclusion: {
      headline: (st.recommend && st.recommend.headline) || (st.scopedResult && st.scopedResult.summary) || bossHeadline || null,
      recommend: st.recommend,
      consequence: st.consequence,
      discovery: st.discovery,
      reportModel: st.reportModel,
      hasEngineResult,
      hasExploratoryOnly: st.exploratoryCalls > 0 && !hasEngineResult
    }
  }
}

/** 每条剧本路径都终止于 archived —— 见 shangyi-core/src/phase_graph.rs 的 assert_well_formed()
 *  （`assert_eq!(*self.path.last().unwrap(), SessionPhase::Archived)`）。 */
const TERMINAL_PHASE = 'archived'

const PHASE_ORDER_INDEX = {
  intake: 0,
  intel: 1,
  model_build: 2,
  solve: 3,
  select: 4,
  consequence: 5,
  discover: 6,
  recommend: 7,
  report: 8,
  archived: 9
}

function inferScope(events) {
  let scope = null
  const started = new Map()
  for (const e of events) {
    if (!e || !e.type) continue
    if (e.type === 'tool_call_started' && e.call_id) started.set(e.call_id, e.name)
    else if (e.type === 'task_scope_selected' && e.scope) scope = e.scope
    else if (e.type === 'scoped_task_completed' && e.result && e.result.scope) scope = e.result.scope
    else if (e.type === 'tool_call_finished' && e.call_id) {
      const name = started.get(e.call_id)
      const outcome = e.outcome || {}
      if ((outcome.status || '') === 'ok') {
        if (name === 'select_task_scope') {
          try {
            const parsed = JSON.parse(outcome.output || '')
            if (parsed && parsed.scope) scope = parsed.scope
          } catch {}
        } else if (name === 'conditional_analysis') {
          let parsed = null
          try { parsed = JSON.parse(outcome.output || '') } catch {}
          scope = (parsed && parsed.scope) || 'conditional'
        }
      }
    } else if (e.type === 'phase_rewound' && e.to === 'intake' && !isRollbackRewind(e.from, e.to, e.due_to)) {
      scope = null
    } else if (e.type === 'followup_task_started') scope = null
  }
  return scope
}

/**
 * 从 case.jsonl 的事件序列推「最新阶段 + 状态」。三条判据都可核，判据不成立即 null：
 *
 *   1) phase     = 最后一个 `phase_entered.to` / `phase_rewound.to`（`followup_task_started` 回到 `intake`）。旧案例没有阶段事件 → null。
 *   2) done      = phase === 'archived'（阶段机不变的终态，见上方常量）。
 *   3) waiting   = 有 `checkpoint_opened` 在最后一次同 id 的 `checkpoint_answered` 之后
 *                  且未因 `phase_rewound` / `followup_task_started` 撤销 —— 正常关口等待用户确认；
 *   4) stuck     = 当前阶段/任务轮次最后一个 `tool_call_finished.outcome.status === 'failed'`
 *                  —— 硬失败收尾（`phase_rewound` 与 `followup_task_started` 会清除历史失败残留）。
 *   5) running   = 最后一次 `turn_started` 没有对应的 `turn_ended`（轮次未收尾）。
 *                  口径说明：日志看不出「进程还在跑」还是「进程被杀」，两者都表现为
 *                  未收尾轮次；因此该值应读作「未收尾」，不是「正在运行」。UI 需区分时可读
 *                  openTurn / lastEvent 两个辅助字段。
 *   6) null      = 以上都不成立（例如空日志、或无阶段事件且无失败）。
 *
 * 辅助字段（新增，供 UI 显示更细的状态，不参与 status 判定）：
 *   pendingCheckpoint  未结算的 checkpoint id
 *   openTurn           是否有未收尾轮次
 *   lastEvent          最后一个事件类型
 *   lastToolOk         最后一次工具调用是否 ok（无工具调用 → null）
 */
export function inferProgress(events) {
  let phase = null
  let scope = null
  let hasConditional = false
  const started = new Map()
  const openMap = new Map()
  const openOrder = []
  let lastTool = null
  let openTurn = false
  for (const e of events) {
    if (!e || !e.type) continue
    if (e.type === 'phase_entered' && e.to) phase = e.to
    else if (e.type === 'task_scope_selected' && e.scope) {
      scope = e.scope
      if (e.scope === 'conditional') hasConditional = true
    } else if (e.type === 'scoped_task_completed' && e.result) {
      if (e.result.scope) {
        scope = e.result.scope
        if (e.result.scope === 'conditional') hasConditional = true
      }
      if (e.result.conditional) hasConditional = true
    } else if (e.type === 'phase_rewound' && e.to) {
      phase = e.to
      lastTool = null
      const targetIdx = PHASE_ORDER_INDEX[e.to]
      const rollback = isRollbackRewind(e.from, e.to, e.due_to)
      const clearTarget = !rollback
      const cutoff = typeof targetIdx === 'number' ? (clearTarget ? targetIdx : targetIdx + 1) : null
      if (cutoff !== null && cutoff <= 7) hasConditional = false
      if (typeof targetIdx === 'number' && targetIdx === 0 && clearTarget) scope = null
      for (let i = openOrder.length - 1; i >= 0; i--) {
        const id = openOrder[i]
        const cp = openMap.get(id)
        const cpIdx = cp && cp.phase ? PHASE_ORDER_INDEX[cp.phase] : undefined
        if (
          cutoff !== null &&
          (typeof cpIdx === 'number' ? cpIdx >= cutoff : (e.to === 'intake' && clearTarget))
        ) {
          openMap.delete(id)
          openOrder.splice(i, 1)
        }
      }
    } else if (e.type === 'followup_task_started') {
      const wasConditional =
        scope === 'conditional' ||
        hasConditional ||
        e.prior_scope === 'conditional' ||
        e.scope === 'conditional' ||
        e.task_scope === 'conditional'
      scope = null
      hasConditional = false
      lastTool = null
      if (wasConditional) {
        phase = 'intake'
        openMap.clear()
        openOrder.length = 0
      }
    } else if (e.type === 'tool_call_started' && e.call_id) {
      started.set(e.call_id, e.name)
    } else if (e.type === 'checkpoint_opened' && e.id) {
      openMap.set(e.id, {
        id: String(e.id),
        phase: e.phase || phase || null,
        items: Array.isArray(e.items) ? e.items.map((x) => String(x)) : [],
        at: e.at || null
      })
      const idx = openOrder.indexOf(e.id)
      if (idx >= 0) openOrder.splice(idx, 1)
      openOrder.push(e.id)
    } else if (e.type === 'checkpoint_answered' && e.id) {
      const first = Array.isArray(e.answers) && e.answers.length ? String(e.answers[0] || '') : ''
      // 与 Rust 端（shangyi-cli/src/run.rs & shangyi-mcp）口径一致：「追问：…」不关闭待决关口
      if (!first.startsWith('追问：')) {
        openMap.delete(e.id)
        const idx = openOrder.indexOf(e.id)
        if (idx >= 0) openOrder.splice(idx, 1)
      }
    } else if (e.type === 'tool_call_finished') {
      lastTool = e
      const name = e.call_id ? started.get(e.call_id) : null
      const outcome = e.outcome || {}
      if ((outcome.status || '') === 'ok') {
        if (name === 'select_task_scope') {
          try {
            const parsed = JSON.parse(outcome.output || '')
            if (parsed && parsed.scope) {
              scope = parsed.scope
              if (parsed.scope === 'conditional') hasConditional = true
            }
          } catch {}
        } else if (name === 'conditional_analysis') {
          let parsed = null
          try { parsed = JSON.parse(outcome.output || '') } catch {}
          scope = (parsed && parsed.scope) || 'conditional'
          if ((parsed && parsed.conditional) || scope === 'conditional') hasConditional = true
        }
      }
    }
    else if (e.type === 'turn_started') openTurn = true
    else if (e.type === 'turn_ended') openTurn = false
  }
  const pendingCheckpointDetail = openOrder.length ? (openMap.get(openOrder[openOrder.length - 1]) || null) : null
  const pendingCheckpoint = pendingCheckpointDetail ? pendingCheckpointDetail.id : null
  const lastToolOk = lastTool ? (lastTool.outcome || {}).status === 'ok' : null
  const lastToolFailed = lastTool ? (lastTool.outcome || {}).status === 'failed' : false

  let status = null
  if (phase === TERMINAL_PHASE) status = 'done'
  else if (pendingCheckpoint) status = 'waiting'
  else if (lastToolFailed) status = 'stuck'
  else if (openTurn) status = 'running'

  return {
    phase,
    status,
    pendingCheckpoint,
    pendingCheckpointDetail,
    openTurn,
    lastEvent: events.length ? events[events.length - 1].type : null,
    lastToolOk
  }
}

/** 案例列表摘要（给前端做案例选择器 / 工作台案例列表用）。只读、有上限，避免一次扫全库。 */
export function summarizeCases(limit = 30, home = defaultHome()) {
  const out = []
  for (const c of listCases(home)) {
    if (c.size === 0) continue
    let events = []
    try { events = readCase(c.log).events } catch { continue }
    const names = new Set(events.filter((e) => e.type === 'tool_call_started').map((e) => e.name))
    let mode = null
    let playbook = null
    try {
      const header = JSON.parse(fs.readFileSync(path.join(c.dir, 'header.json'), 'utf8'))
      mode = header.mode || null
      playbook = header.playbook_id || null
    } catch { /* 可选 */ }
    const scope = inferScope(events)
    const progress = inferProgress(events)
    out.push({
      caseId: c.caseId,
      mtime: new Date(c.mtime).toISOString(),
      mode,
      playbook,
      scope,
      events: events.length,
      hasMatrix: names.has('gamespec_build'),
      hasSolve: names.has('solve_equilibria'),
      hasSelection: names.has('select_fuse') || names.has('select_equilibria'),
      // 新增（本版）：阶段与状态；判据见 inferProgress 注释
      phase: progress.phase,
      status: progress.status,
      pendingCheckpoint: progress.pendingCheckpoint,
      openTurn: progress.openTurn,
      lastEvent: progress.lastEvent,
      lastToolOk: progress.lastToolOk
    })
    if (out.length >= limit) break
  }
  return out
}

/** 「最新且含博弈结构」的案例（前端默认用它，因为它才有支付矩阵/均衡卡可看）。 */
export function latestCaseWithCards(home = defaultHome()) {
  const cases = summarizeCases(30, home)
  return (cases.find((c) => c.hasMatrix || c.hasSolve) || cases[0] || null)
}

/**
 * 受控真人关口结算（Option 1A 通道一）：仅响应经 DSH Web 鉴权的真人点击，
 * 按 CaseLog 追加契约（seq 单调递增 + fsync）写入 `user_input_recorded`（如有备注）
 * 与 `checkpoint_answered(user_actor="user")`，随后运行中的 `shangyi-mcp` 在下一次
 * `tools/call` 时自动增量同步推进阶段机。
 */
export function answerCheckpoint(payload, home = defaultHome()) {
  if (!payload || typeof payload !== 'object') {
    return { ok: false, status: 400, error: '请求体必须是 JSON 对象' }
  }
  const caseRef = typeof payload.caseId === 'string' ? payload.caseId.trim() : ''
  if (!caseRef) {
    return { ok: false, status: 400, error: 'caseId 不能为空' }
  }
  const decision = payload.decision
  if (decision !== 'approve' && decision !== 'reject') {
    return { ok: false, status: 400, error: 'decision 只能是 "approve" 或 "reject"' }
  }
  const expectedCpId = typeof payload.checkpointId === 'string' && payload.checkpointId.trim()
    ? payload.checkpointId.trim()
    : null
  const rawNote = typeof payload.note === 'string' ? payload.note.trim() : ''
  if (rawNote.length > 2000) {
    return { ok: false, status: 400, error: '备注长度不能超过 2000 字' }
  }

  let target
  try {
    target = resolveCaseTarget(caseRef, home)
  } catch (e) {
    return { ok: false, status: 404, error: String(e?.message || e) }
  }

  const lockPath = path.join(target.dir, '.case.lock')
  let lockFd
  try {
    try {
      lockFd = fs.openSync(lockPath, 'wx', 0o600)
    } catch (lockErr) {
      if (lockErr && lockErr.code === 'EEXIST') {
        try {
          const st = fs.statSync(lockPath)
          if (Date.now() - st.mtimeMs > 10000) {
            fs.unlinkSync(lockPath)
            lockFd = fs.openSync(lockPath, 'wx', 0o600)
          }
        } catch { /* ignore stale lock race */ }
      }
      if (lockFd === undefined) {
        return { ok: false, status: 409, error: '该案例正在处理另一笔关口写入，请稍后重试' }
      }
    }

    let parsed
    try {
      parsed = readCase(target.log)
    } catch (e) {
      return { ok: false, status: 500, error: `读取案例日志失败：${String(e?.message || e)}` }
    }
    if (parsed.badLines > 0) {
      return { ok: false, status: 500, error: `案例日志存在 ${parsed.badLines} 行损坏记录，拒绝追加` }
    }

    const progress = inferProgress(parsed.events)
    const openCp = progress.pendingCheckpointDetail
    if (!openCp) {
      return { ok: false, status: 409, error: `案例 \`${target.caseId}\` 当前没有处于待确认状态的关口` }
    }
    if (expectedCpId && expectedCpId !== openCp.id) {
      return {
        ok: false,
        status: 409,
        error: `待决关口已变更为 \`${openCp.id}\`（请求的是 \`${expectedCpId}\`），请刷新后重试`
      }
    }

    const at = new Date().toISOString()
    let seq = parsed.maxSeq
    const lines = []
    if (rawNote) {
      seq += 1
      lines.push(JSON.stringify({
        seq,
        event: {
          type: 'user_input_recorded',
          text: rawNote,
          user_actor: 'user',
          at
        }
      }))
    }
    const answerLine = decision === 'approve'
      ? (rawNote ? `确认：${rawNote}` : '确认')
      : (rawNote ? `修改：${rawNote}` : '修改：用户要求修订当前阶段内容')
    seq += 1
    lines.push(JSON.stringify({
      seq,
      event: {
        type: 'checkpoint_answered',
        id: openCp.id,
        answers: [answerLine],
        user_actor: 'user',
        at
      }
    }))

    let fd
    let origSize = null
    try {
      origSize = fs.statSync(target.log).size
      fd = fs.openSync(target.log, 'a')
      fs.writeSync(fd, lines.join('\n') + '\n', null, 'utf8')
      fs.fsyncSync(fd)
    } catch (e) {
      if (fd !== undefined && origSize !== null) {
        try { fs.ftruncateSync(fd, origSize) } catch { /* ignore rollback error */ }
      }
      return { ok: false, status: 500, error: `写入案例日志失败：${String(e?.message || e)}` }
    } finally {
      if (fd !== undefined) {
        try { fs.closeSync(fd) } catch { /* ignore */ }
      }
    }

    return {
      ok: true,
      caseId: target.caseId,
      checkpointId: openCp.id,
      phase: openCp.phase,
      decision,
      answer: answerLine,
      seq,
      at
    }
  } finally {
    if (lockFd !== undefined) {
      try { fs.closeSync(lockFd) } catch { /* ignore */ }
      try { fs.unlinkSync(lockPath) } catch { /* ignore */ }
    }
  }
}

/** 提取 DSH `session/event` 中真实用户发出的纯文本（默认拒绝：严格要求 `data.source.kind === 'user'`）。 */
export function extractTrustedUserMessage(event) {
  if (!event || event.type !== 'user/message') return null
  const data = event.data || {}
  if (!data.source || typeof data.source !== 'object' || data.source.kind !== 'user') return null
  const raw = data.content !== undefined ? data.content : data.text
  let text = ''
  if (typeof raw === 'string') {
    text = raw
  } else if (Array.isArray(raw)) {
    text = raw
      .filter((p) => p && (p.type === 'text' || typeof p.text === 'string'))
      .map((p) => String(p.text || ''))
      .join('\n')
  }
  const trimmed = text.trim()
  return trimmed ? trimmed : null
}

/** 将真实用户消息追加到 `$SHANGYI_HOME/runtime/trusted_user_inputs.jsonl`，供 `shangyi-mcp` 增量核验。 */
export function recordTrustedUserInput(text, sessionId = null, home = defaultHome()) {
  const trimmed = typeof text === 'string' ? text.trim() : ''
  if (!trimmed) return false
  const runtimeDir = path.join(home, 'runtime')
  fs.mkdirSync(runtimeDir, { recursive: true })
  const ledgerPath = path.join(runtimeDir, 'trusted_user_inputs.jsonl')
  const line = JSON.stringify({
    session_id: sessionId || null,
    text: trimmed,
    user_actor: 'user',
    unix_secs: Math.floor(Date.now() / 1000),
    at: new Date().toISOString()
  }) + '\n'
  fs.appendFileSync(ledgerPath, line, 'utf8')
  return true
}

// ────────────────────────────────────────────────────────────────────────────
// 知识库（仓库内 shangyi/kb，只读）
//
// 口径来源（实测 2026-11-11，35 篇）：
//   - 目录是**一层平铺**，没有 theory/ method/ concept 子目录；
//   - 每篇开头有 YAML front-matter：id / name / kind（theory|method|concept）/ summary / tags；
//   - front-matter 之后的第一行 `#` 是**章节标题**（如「为什么这一步不能交给模型自由发挥」），
//     不是文档标题。
// 因此：
//   - group 取 front-matter 的 `kind`（有子目录时回退到第一级目录名）；
//   - title 取 front-matter 的 `name`；没有时才回退到首个 `#` 标题、再回退文件名。
//     ⚠️ 契约 §2.3 原文写的是「title=首个 # 标题」；按实测该字段会变成章节标题，
//     故按上面的优先级实现，并把首个 `#` 标题另存为 heading 字段（不丢信息）。
// ────────────────────────────────────────────────────────────────────────────

/** 解析文件开头的 YAML front-matter（只认 `key: value` 这种标量行，不引 YAML 依赖）。 */
function parseFrontMatter(text) {
  const out = {}
  if (!text.startsWith('---')) return out
  const end = text.indexOf('\n---', 3)
  if (end < 0) return out
  for (const line of text.slice(3, end).split('\n')) {
    const m = /^([A-Za-z_][\w-]*):[ \t]*(.*)$/.exec(line.trim())
    if (!m) continue
    let v = m[2].trim()
    if ((v.startsWith('"') && v.endsWith('"')) || (v.startsWith("'") && v.endsWith("'"))) v = v.slice(1, -1)
    out[m[1]] = v
  }
  return out
}

/** 首个一级标题（`# xxx`），跳过代码块。 */
function firstHeading(text) {
  let fenced = false
  for (const line of text.split('\n')) {
    if (/^```/.test(line.trim())) { fenced = !fenced; continue }
    if (fenced) continue
    const m = /^#[ \t]+(.+?)\s*$/.exec(line)
    if (m) return m[1].trim()
  }
  return null
}

/** 相对路径 → slug：去掉 .md、统一成 posix 分隔符。 */
function slugOf(relPath) {
  return relPath.split(path.sep).join('/').replace(/\.md$/, '')
}

/** 递归收集 kb 下的 .md（相对路径）。排序保证列表稳定。 */
function walkMd(dir, prefix = '', out = []) {
  let entries = []
  try { entries = fs.readdirSync(dir, { withFileTypes: true }) } catch { return out }
  for (const ent of entries) {
    if (ent.name.startsWith('.')) continue
    const abs = path.join(dir, ent.name)
    const rel = prefix ? prefix + '/' + ent.name : ent.name
    if (ent.isDirectory()) walkMd(abs, rel, out)
    else if (ent.isFile() && ent.name.endsWith('.md')) out.push(rel)
  }
  return out
}

/**
 * 知识库索引（合并仓库内 shangyi/kb 与用户 $SHANGYI_HOME/kb）。
 * @param {{kbDir?: string|null, home?: string}} opts 可选注入 kb 目录（测试用）；缺省用仓库内 shangyi/kb。
 * @returns {{ok: boolean, docs: Array, error?: string}}
 */
export function listKb(opts = {}) {
  const dir = opts.kbDir !== undefined ? opts.kbDir : KB_DIR
  if (!dir) return { ok: false, docs: [], error: '找不到仓库内的 shangyi/kb（可设 SHANGYI_ROOT 指定仓库根）' }

  const dirs = [{ root: dir, userAdded: false }]
  if (opts.kbDir === undefined) {
    const userKb = path.join(opts.home !== undefined ? opts.home : defaultHome(), 'kb')
    try {
      if (fs.statSync(userKb).isDirectory()) dirs.push({ root: userKb, userAdded: true })
    } catch { /* 用户扩展库可选 */ }
  }

  const bySlug = new Map()
  for (const entry of dirs) {
    const files = walkMd(entry.root).sort()
    for (const rel of files) {
      let text = ''
      try { text = fs.readFileSync(path.join(entry.root, rel), 'utf8') } catch { continue }
      const fm = parseFrontMatter(text)
      const heading = firstHeading(text)
      const base = path.basename(rel, '.md')
      const firstDir = rel.includes('/') ? rel.split('/')[0] : null
      const slug = slugOf(rel)
      bySlug.set(slug, {
        slug,
        // title 来源优先级：front-matter.name > 首个 # 标题 > 文件名（见本节顶部口径说明）
        title: fm.name || heading || base,
        group: fm.kind || firstDir || null,
        path: (entry.userAdded ? '~/.shangyi/kb/' : 'shangyi/kb/') + rel.split(path.sep).join('/'),
        id: fm.id || base,
        kind: fm.kind || null,
        summary: fm.summary || null,
        tags: fm.tags || null,
        heading,
        userAdded: entry.userAdded
      })
    }
  }
  const docs = Array.from(bySlug.values()).sort((a, b) => a.slug.localeCompare(b.slug))
  return { ok: true, docs }
}

/** slug 合法性：非空、无 NUL、非绝对路径、无 . / .. 段、扩展名只能是 .md（可省略）。 */
function validateSlug(slug) {
  if (typeof slug !== 'string' || !slug.trim()) return { ok: false, status: 400, error: 'slug 不能为空' }
  if (slug.includes('\0')) return { ok: false, status: 400, error: 'slug 含非法字符' }
  if (slug.startsWith('/') || /^[A-Za-z]:/.test(slug)) return { ok: false, status: 400, error: 'slug 不能是绝对路径' }
  const parts = slug.split('/')
  for (const p of parts) {
    if (p === '' || p === '.' || p === '..') return { ok: false, status: 400, error: 'slug 含非法路径段' }
  }
  if (slug.endsWith('.md')) return { ok: true, bare: slug.slice(0, -3) }
  if (/\.[A-Za-z0-9]+$/.test(slug)) return { ok: false, status: 400, error: '只允许读 .md' }
  return { ok: true, bare: slug }
}

function readKbDocFromRoot(rootDir, bare, userAdded) {
  const abs = path.join(rootDir, bare + '.md')
  let real
  try { real = fs.realpathSync(abs) } catch { return { ok: false, status: 404, error: '没有这篇文档' } }
  let realRoot
  try { realRoot = fs.realpathSync(rootDir) } catch { return { ok: false, status: 500, error: '知识库目录不可读' } }
  if (real !== realRoot && !real.startsWith(realRoot + path.sep)) {
    return { ok: false, status: 400, error: '路径越界' }
  }
  if (!real.endsWith('.md')) return { ok: false, status: 400, error: '只允许读 .md' }
  let markdown
  try { markdown = fs.readFileSync(real, 'utf8') } catch { return { ok: false, status: 404, error: '没有这篇文档' } }
  const fm = parseFrontMatter(markdown)
  const heading = firstHeading(markdown)
  return {
    ok: true,
    slug: slugOf(path.relative(rootDir, real)),
    title: fm.name || heading || path.basename(real, '.md'),
    markdown,
    group: fm.kind || null,
    id: fm.id || null,
    summary: fm.summary || null,
    tags: fm.tags || null,
    heading,
    userAdded
  }
}

/**
 * 读单篇正文。越界（.. / 绝对路径 / 非 .md / 符号链接逃逸）一律拒绝。
 * @param {string} slug 形如 theory/xxx 或 xxx（不带 .md 亦可）
 * @param {{kbDir?: string|null, home?: string}} opts 测试注入
 */
export function readKbDoc(slug, opts = {}) {
  const dir = opts.kbDir !== undefined ? opts.kbDir : KB_DIR
  if (!dir) return { ok: false, status: 500, error: '找不到仓库内的 shangyi/kb' }
  const v = validateSlug(slug)
  if (!v.ok) return { ok: false, status: v.status, error: v.error }

  if (opts.kbDir === undefined) {
    const userKb = path.join(opts.home !== undefined ? opts.home : defaultHome(), 'kb')
    try {
      if (fs.statSync(userKb).isDirectory()) {
        const userHit = readKbDocFromRoot(userKb, v.bare, true)
        if (userHit.ok || userHit.status === 400) return userHit
      }
    } catch { /* ignore */ }
  }
  return readKbDocFromRoot(dir, v.bare, false)
}

function isExecutableFile(filePath) {
  if (!filePath || typeof filePath !== 'string') return false
  try {
    const st = fs.statSync(filePath)
    if (!st.isFile()) return false
    fs.accessSync(filePath, fs.constants.X_OK)
    return true
  } catch {
    return false
  }
}

function checkBinaryHealth(opts, home) {
  if (opts.binPath !== undefined) {
    if (isExecutableFile(opts.binPath)) {
      return { found: true, path: opts.binPath, source: 'override', error: null }
    }
    return {
      found: false,
      path: opts.binPath || null,
      source: 'missing',
      error: opts.binPath
        ? `指定的 shangyi 二进制不可执行或不存在：${opts.binPath}`
        : '未检测到可执行 shangyi 二进制'
    }
  }
  const envBin = process.env.SHANGYI_BIN && process.env.SHANGYI_BIN.trim()
  if (envBin) {
    if (isExecutableFile(envBin)) {
      return { found: true, path: envBin, source: 'env', error: null }
    }
    return {
      found: false,
      path: envBin,
      source: 'missing',
      error: `环境变量 SHANGYI_BIN 指向的文件不可执行或不存在：${envBin}`
    }
  }
  const candidates = [
    { path: path.join(home, 'bin', 'shangyi'), source: 'home' },
    { path: path.join(os.homedir(), '.shangyi', 'bin', 'shangyi'), source: 'home' },
    { path: '/tmp/shangyi-target/release/shangyi', source: 'target' }
  ]
  const pathEnv = process.env.PATH || ''
  for (const dir of pathEnv.split(path.delimiter)) {
    if (dir && dir.trim()) candidates.push({ path: path.join(dir.trim(), 'shangyi'), source: 'path' })
  }
  for (const cand of candidates) {
    if (isExecutableFile(cand.path)) {
      return { found: true, path: cand.path, source: cand.source, error: null }
    }
  }
  return {
    found: false,
    path: null,
    source: 'missing',
    error: '在 SHANGYI_BIN、~/.shangyi/bin/shangyi、/tmp/shangyi-target/release/shangyi 与 PATH 中均未找到可执行 shangyi 二进制。'
  }
}

function inspectMcpToolsHealth(opts) {
  if (Array.isArray(opts.mcpToolNames)) {
    const hit = opts.mcpToolNames.filter((n) => String(n).startsWith('mcp__shangyi__'))
    return {
      status: hit.length > 0 ? 'ok' : 'missing',
      count: hit.length,
      sample: hit.slice(0, 8)
    }
  }
  if (typeof opts.mcpToolCount === 'number') {
    return {
      status: opts.mcpToolCount > 0 ? 'ok' : 'missing',
      count: opts.mcpToolCount,
      sample: []
    }
  }
  const readService = (obj, svc) => {
    if (!obj) return undefined
    if (typeof obj.get === 'function') {
      try {
        const hit = obj.get(svc)
        if (hit !== undefined) return hit
      } catch { /* ignore */ }
    }
    try { return obj[svc] } catch { return undefined }
  }
  const runtimes = [
    opts.toolsRuntime,
    readService(opts.c, 'tools'),
    readService(opts.ctx, 'tools'),
    readService(opts.c && opts.c.root, 'tools'),
    readService(opts.ctx && opts.ctx.root, 'tools')
  ]
  for (const rt of runtimes) {
    if (!rt || typeof rt.view !== 'function') continue
    try {
      const v = rt.view()
      const keys = []
      if (v && v.visible && typeof v.visible.keys === 'function') {
        for (const k of v.visible.keys()) keys.push(String(k))
      } else if (v && v.all && typeof v.all.keys === 'function') {
        for (const k of v.all.keys()) keys.push(String(k))
      }
      const hit = keys.filter((k) => k.startsWith('mcp__shangyi__'))
      return {
        status: hit.length > 0 ? 'ok' : 'missing',
        count: hit.length,
        sample: hit.slice(0, 8)
      }
    } catch { /* ignore broken runtime view */ }
  }
  return { status: 'unknown', count: null, sample: [] }
}

/**
 * 商奕确定性内核健康检查：检查 shangyi 可执行文件、MCP 工具注册状态、知识库与持久化案例目录。
 */
export function getHealthStatus(opts = {}) {
  const home = opts.home !== undefined ? opts.home : defaultHome()
  const binary = checkBinaryHealth(opts, home)
  const kbRes = listKb({ kbDir: opts.kbDir, home })
  const kb = {
    ok: Boolean(kbRes.ok && kbRes.docs && kbRes.docs.length > 0),
    dir: opts.kbDir !== undefined ? opts.kbDir : KB_DIR,
    docCount: Array.isArray(kbRes.docs) ? kbRes.docs.length : 0,
    error: kbRes.ok ? (kbRes.docs.length ? null : '知识库目录为空') : (kbRes.error || '知识库不可用')
  }
  const casesDir = path.join(home, 'cases')
  let casesWritable = false
  try {
    const targetDir = fs.existsSync(casesDir) ? casesDir : (fs.existsSync(home) ? home : path.dirname(home))
    fs.accessSync(targetDir, fs.constants.W_OK)
    casesWritable = true
  } catch { casesWritable = false }
  const workspace = {
    home,
    casesDir,
    casesWritable,
    caseCount: listCases(home).length
  }
  const mcpTools = inspectMcpToolsHealth(opts)

  const problems = []
  if (!binary.found) {
    problems.push({
      code: 'missing_binary',
      severity: 'blocking',
      title: '未找到商奕确定性求解内核（shangyi 二进制）',
      detail: `${binary.error} 缺少二进制时，博弈求解、成本精算、报告生成工具均无法运行；若强行对话将退化为纯模型自由发挥，违反铁律 1。`,
      fixCommands: [
        'tools/loop/build.sh build --release -p shangyi-cli',
        'cargo install --path shangyi/crates/shangyi-cli --root ~/.shangyi',
        'shangyi start'
      ]
    })
  }
  if (mcpTools.status === 'missing') {
    problems.push({
      code: 'mcp_tools_missing',
      severity: 'blocking',
      title: 'MCP 确定性工具组（mcp__shangyi__*）未连接',
      detail: '宿主工具表中当前有 0 个商奕确定性工具可用。请勿在无工具状态下继续商业或金融决策对话。',
      fixCommands: [
        'shangyi start',
        'bash shangyi/ui/scripts/install-workbench.sh --no-open'
      ]
    })
  } else if (mcpTools.status === 'unknown') {
    problems.push({
      code: 'mcp_tools_unknown',
      severity: 'blocking',
      title: '未能核实 MCP 确定性工具组（mcp__shangyi__*）注册状态',
      detail: '宿主上下文未提供可读的工具注册视图（tools.view()），无法确认确定性领域工具是否已连接；在确认前请勿依赖对话完成定量分析。',
      fixCommands: [
        'shangyi start',
        'bash shangyi/ui/scripts/install-workbench.sh --no-open'
      ]
    })
  }
  if (!kb.ok) {
    problems.push({
      code: 'kb_unavailable',
      severity: 'warning',
      title: '未能加载商奕本地知识库（shangyi/kb）',
      detail: kb.error,
      fixCommands: ['export SHANGYI_ROOT=/path/to/shangyi-repo']
    })
  }

  return {
    ok: Boolean(binary.found && kb.ok && mcpTools.status === 'ok'),
    checkedAt: new Date().toISOString(),
    binary,
    kb,
    workspace,
    mcpTools,
    problems
  }
}

export function apply(ctx) {
  if (ctx && typeof ctx.on === 'function') {
    ctx.on('session/event', (session, event) => {
      try {
        const text = extractTrustedUserMessage(event)
        if (!text) return
        const sid = session && (session.id || session.sessionId || null)
        recordTrustedUserInput(text, sid)
      } catch (e) {
        ctx.logger?.warn?.(`dsh-plugin-shangyi: 写入 trusted_user_inputs 失败：${String(e?.message || e)}`)
      }
    })
  }

  ctx.inject(['connection'], (c) => {
    const conn = c.get('connection')
    const fetchReg = conn && conn.fetch
    if (!fetchReg || typeof fetchReg.register !== 'function') {
      c.logger?.warn?.('dsh-plugin-shangyi: connection.fetch 不可用，卡片路由未注册')
      return
    }
    const startupHealth = getHealthStatus({ c, ctx })
    if (!startupHealth.ok) {
      c.logger?.error?.(
        `dsh-plugin-shangyi: 确定性内核健康检查未通过 [${startupHealth.problems.map((p) => p.code).join(', ')}] — 请执行 shangyi start 或检查 SHANGYI_BIN`
      )
    }
    c.effect(
      () => fetchReg.register({
        path: '/api/shangyi/health',
        methods: ['GET'],
        requestBody: 'buffered',
        fetch: async () => {
          try {
            const h = getHealthStatus({ c, ctx })
            return Response.json(h, { headers: { 'cache-control': 'no-store' } })
          } catch (e) {
            return Response.json(
              { ok: false, error: String(e?.message || e), problems: [{ code: 'health_error', severity: 'blocking', title: '健康检查失败', detail: String(e?.message || e), fixCommands: ['shangyi start'] }] },
              { status: 500, headers: { 'cache-control': 'no-store' } }
            )
          }
        }
      }),
      'dsh-plugin-shangyi: health route'
    )
    c.effect(
      () => fetchReg.register({
        path: '/api/shangyi/cards',
        methods: ['GET'],
        requestBody: 'buffered',
        fetch: async (request) => {
          try {
            const url = new URL(request.url)
            const ref = url.searchParams.get('case') || 'latest-cards'
            const resolved = ref === 'latest-cards' ? ((latestCaseWithCards() || {}).caseId || 'latest') : ref
            return Response.json({ ok: true, requested: ref, ...projectCase(resolved) }, { headers: { 'cache-control': 'no-store' } })
          } catch (e) {
            return Response.json({ ok: false, error: String(e?.message || e) }, { status: 500 })
          }
        }
      }),
      'dsh-plugin-shangyi: cards route'
    )
    c.effect(
      () => fetchReg.register({
        path: '/api/shangyi/cases',
        methods: ['GET'],
        requestBody: 'buffered',
        fetch: async () => {
          try {
            return Response.json({ ok: true, cases: summarizeCases(30) }, { headers: { 'cache-control': 'no-store' } })
          } catch (e) {
            return Response.json({ ok: false, error: String(e?.message || e) }, { status: 500 })
          }
        }
      }),
      'dsh-plugin-shangyi: cases route'
    )
    // 受控真人关口确认路由：经 DSH Web 鉴权后向 case.jsonl 追加 CheckpointAnswered(user_actor="user")。
    c.effect(
      () => fetchReg.register({
        path: '/api/shangyi/checkpoint',
        methods: ['POST'],
        requestBody: 'buffered',
        fetch: async (request) => {
          const ct = String(request.headers.get('content-type') || '').toLowerCase()
          if (!ct.startsWith('application/json')) {
            return Response.json({ ok: false, error: 'Content-Type 必须为 application/json' }, { status: 415, headers: { 'cache-control': 'no-store' } })
          }
          const origin = request.headers.get('origin')
          if (origin) {
            try {
              if (new URL(origin).host !== new URL(request.url).host) {
                return Response.json({ ok: false, error: '拒绝跨源请求' }, { status: 403, headers: { 'cache-control': 'no-store' } })
              }
            } catch {
              return Response.json({ ok: false, error: '非法 Origin 头' }, { status: 403, headers: { 'cache-control': 'no-store' } })
            }
          }
          let body = null
          try {
            body = await request.json()
          } catch {
            return Response.json({ ok: false, error: '请求体不是合法 JSON' }, { status: 400, headers: { 'cache-control': 'no-store' } })
          }
          const r = answerCheckpoint(body)
          const status = r.ok ? 200 : (r.status || 500)
          return Response.json(r, { status, headers: { 'cache-control': 'no-store' } })
        }
      }),
      'dsh-plugin-shangyi: checkpoint route'
    )
    // 知识库索引：只读、只 GET。
    c.effect(
      () => fetchReg.register({
        path: '/api/shangyi/kb',
        methods: ['GET'],
        requestBody: 'buffered',
        fetch: async () => {
          const r = listKb()
          if (!r.ok) return Response.json(r, { status: 500, headers: { 'cache-control': 'no-store' } })
          return Response.json(r, { headers: { 'cache-control': 'no-store' } })
        }
      }),
      'dsh-plugin-shangyi: kb index route'
    )
    // 单篇正文：越界由 readKbDoc 的校验返回 400/404，不泄露文件系统。
    c.effect(
      () => fetchReg.register({
        path: '/api/shangyi/kb/doc',
        methods: ['GET'],
        requestBody: 'buffered',
        fetch: async (request) => {
          const url = new URL(request.url)
          const r = readKbDoc(url.searchParams.get('slug') || '')
          const status = r.ok ? 200 : (r.status || 500)
          return Response.json(r, { status, headers: { 'cache-control': 'no-store' } })
        }
      }),
      'dsh-plugin-shangyi: kb doc route'
    )
  })
}
