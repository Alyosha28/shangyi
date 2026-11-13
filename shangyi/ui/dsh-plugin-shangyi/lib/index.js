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

/** 把一次 gamespec_build 的入参解析成 spec（兼容模型把参数塞进 arguments 字符串的形状）。 */
function unwrapSpec(argsRaw) {
  if (!argsRaw) return null
  let v
  try { v = JSON.parse(argsRaw) } catch { return null }
  // 实测存在 {"arguments":"{...}"} 的双层形状
  for (let i = 0; i < 3 && v && typeof v === 'object' && typeof v.arguments === 'string'; i++) {
    try { v = JSON.parse(v.arguments) } catch { break }
  }
  return v && typeof v === 'object' ? v : null
}

/** 合并所有 gamespec_build 入参（后者覆盖前者），得到模型最终想建的博弈骨架。 */
function mergeSpecs(specs) {
  const out = { players: null, strategies: null, payoffs: null, params: null, game_id: null }
  for (const s of specs) {
    if (!s) continue
    if (Array.isArray(s.players) && s.players.length) out.players = s.players
    if (s.strategies && typeof s.strategies === 'object' && Object.keys(s.strategies).length) out.strategies = s.strategies
    if (s.payoffs && typeof s.payoffs === 'object') out.payoffs = s.payoffs
    if (s.params && typeof s.params === 'object') out.params = s.params
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

/** 案例日志 → 卡片数据。纯函数（除读盘外），便于对拍。 */
export function projectCase(caseRef = 'latest', home = defaultHome()) {
  const target = resolveCaseTarget(caseRef, home)

  const { events, badLines } = readCase(target.log)
  const started = new Map()
  const calls = []
  for (const e of events) if (e.type === 'tool_call_started') started.set(e.call_id, e)
  for (const e of events) {
    if (e.type !== 'tool_call_finished') continue
    const s = started.get(e.call_id) || {}
    calls.push({ name: s.name || '?', args: s.arguments || '', outcome: e.outcome || {} })
  }

  const specBuilds = calls.filter((c) => c.name === 'gamespec_build' && c.outcome.status === 'ok').map((c) => unwrapSpec(c.args))
  const spec = mergeSpecs(specBuilds)

  const solves = calls
    .filter((c) => c.name === 'solve_equilibria' && c.outcome.status === 'ok')
    .map((c) => { try { return JSON.parse(c.outcome.output) } catch { return null } })
    .filter(Boolean)
  const solve = solves.length ? solves[solves.length - 1] : null

  const fuses = calls
    .filter((c) => c.name === 'select_fuse' && c.outcome.status === 'ok')
    .map((c) => { try { return JSON.parse(c.outcome.output) } catch { return null } })
    .filter(Boolean)
  const fuse = fuses.length ? fuses[fuses.length - 1] : null

  const failures = calls
    .filter((c) => c.outcome.status !== 'ok')
    .slice(-5)
    .map((c) => ({
      tool: c.name,
      status: c.outcome.status,
      detail: String(c.outcome.error ?? c.outcome.detail ?? c.outcome.output ?? '').slice(0, 600)
    }))

  // 注意：phase_entered 的字段是 from/to（不是 phase）；旧案例没有该事件，phases 为空。
  const phases = events.filter((e) => e.type === 'phase_entered').map((e) => e.to).filter(Boolean)
  const checkpoints = events.filter((e) => e.type === 'checkpoint_answered').map((e) => e.id)
  const scopeEv = [...events].reverse().find((e) => e.type === 'task_scope_selected')
  const tokens = events.filter((e) => e.type === 'token_usage').reduce((acc, e) => acc + (e.input_tokens || 0) + (e.output_tokens || 0), 0)
  const turns = events.filter((e) => e.type === 'turn_started').length
  const progress = inferProgress(events)

  let header = null
  try { header = JSON.parse(fs.readFileSync(path.join(target.dir, 'header.json'), 'utf8')) } catch { /* 老案例可能没有 */ }

  return {
    source: { path: target.log, caseId: target.caseId, mtime: new Date(target.mtime).toISOString(), badLines },
    meta: {
      case_id: header?.case_id || target.caseId,
      mode: header?.mode || null,
      playbook: header?.playbook_id || null,
      created_at: header?.created_at || null,
      events: events.length,
      turns,
      tokens,
      scope: scopeEv?.scope || null,
      phases,
      checkpoints,
      tools: calls.length
    },
    pendingCheckpoint: progress.pendingCheckpoint,
    pendingCheckpointDetail: progress.pendingCheckpointDetail,
    matrix: matrixCard(spec),
    solve: solve ? { completeness: solve.completeness || null, route: solve.route || null, equilibria: solve.equilibria || [], refinement: solve.refinement || null } : null,
    selection: fuse ? { entries: fuse.entries || [], confidence: fuse.confidence || null, divergences: fuse.divergences || [] } : null,
    failures
  }
}

/** 每条剧本路径都终止于 archived —— 见 shangyi-core/src/phase_graph.rs 的 assert_well_formed()
 *  （`assert_eq!(*self.path.last().unwrap(), SessionPhase::Archived)`）。 */
const TERMINAL_PHASE = 'archived'

/**
 * 从 case.jsonl 的事件序列推「最新阶段 + 状态」。三条判据都可核，判据不成立即 null：
 *
 *   1) phase     = 最后一个 `phase_entered.to`。旧案例没有该事件 → null。
 *   2) done      = phase === 'archived'（阶段机不变的终态，见上方常量）。
 *   3) stuck     = 两条可核证据之一：
 *                  a. 有 `checkpoint_opened` 在最后一次同 id 的 `checkpoint_answered` 之后
 *                     —— 闸门在等用户答复，流程无法自行推进；
 *                  b. 最后一个 `tool_call_finished.outcome.status === 'failed'`
 *                     —— 硬失败收尾。注意 `blocked` 是引擎的正当拒绝（如 MCP 侧 web_search
 *                     未接线），不算失败，不计入 stuck。
 *   4) running   = 最后一次 `turn_started` 没有对应的 `turn_ended`（轮次未收尾）。
 *                  口径说明：日志看不出「进程还在跑」还是「进程被杀」，两者都表现为
 *                  未收尾轮次；因此该值应读作「未收尾」，不是「正在运行」。UI 需区分时可读
 *                  openTurn / lastEvent 两个辅助字段。
 *   5) null      = 以上都不成立（例如空日志、或无阶段事件且无失败）。
 *
 * 辅助字段（新增，供 UI 显示更细的状态，不参与 status 判定）：
 *   pendingCheckpoint  未结算的 checkpoint id
 *   openTurn           是否有未收尾轮次
 *   lastEvent          最后一个事件类型
 *   lastToolOk         最后一次工具调用是否 ok（无工具调用 → null）
 */
export function inferProgress(events) {
  let phase = null
  const openMap = new Map()
  const openOrder = []
  let lastTool = null
  let openTurn = false
  for (const e of events) {
    if (!e || !e.type) continue
    if (e.type === 'phase_entered' && e.to) phase = e.to
    else if (e.type === 'checkpoint_opened' && e.id) {
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
    } else if (e.type === 'tool_call_finished') lastTool = e
    else if (e.type === 'turn_started') openTurn = true
    else if (e.type === 'turn_ended') openTurn = false
  }
  const pendingCheckpointDetail = openOrder.length ? (openMap.get(openOrder[openOrder.length - 1]) || null) : null
  const pendingCheckpoint = pendingCheckpointDetail ? pendingCheckpointDetail.id : null
  const lastToolOk = lastTool ? (lastTool.outcome || {}).status === 'ok' : null
  const lastToolFailed = lastTool ? (lastTool.outcome || {}).status === 'failed' : false

  let status = null
  if (phase === TERMINAL_PHASE) status = 'done'
  else if (pendingCheckpoint) status = 'stuck'
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
    const scopeEv = [...events].reverse().find((e) => e.type === 'task_scope_selected')
    const progress = inferProgress(events)
    out.push({
      caseId: c.caseId,
      mtime: new Date(c.mtime).toISOString(),
      mode,
      playbook,
      scope: scopeEv?.scope || null,
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
 * 知识库索引。
 * @param {{kbDir?: string|null}} opts 可选注入 kb 目录（测试用）；缺省用仓库内 shangyi/kb。
 * @returns {{ok: boolean, docs: Array, error?: string}}
 */
export function listKb(opts = {}) {
  const dir = opts.kbDir !== undefined ? opts.kbDir : KB_DIR
  if (!dir) return { ok: false, docs: [], error: '找不到仓库内的 shangyi/kb（可设 SHANGYI_ROOT 指定仓库根）' }
  const files = walkMd(dir).sort()
  const docs = []
  for (const rel of files) {
    let text = ''
    try { text = fs.readFileSync(path.join(dir, rel), 'utf8') } catch { continue }
    const fm = parseFrontMatter(text)
    const heading = firstHeading(text)
    const base = path.basename(rel, '.md')
    const firstDir = rel.includes('/') ? rel.split('/')[0] : null
    docs.push({
      slug: slugOf(rel),
      // title 来源优先级：front-matter.name > 首个 # 标题 > 文件名（见本节顶部口径说明）
      title: fm.name || heading || base,
      group: fm.kind || firstDir || null,
      path: 'shangyi/kb/' + rel.split(path.sep).join('/'),
      id: fm.id || base,
      kind: fm.kind || null,
      summary: fm.summary || null,
      tags: fm.tags || null,
      heading
    })
  }
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

/**
 * 读单篇正文。越界（.. / 绝对路径 / 非 .md / 符号链接逃逸）一律拒绝。
 * @param {string} slug 形如 theory/xxx 或 xxx（不带 .md 亦可）
 * @param {{kbDir?: string|null}} opts 测试注入
 */
export function readKbDoc(slug, opts = {}) {
  const dir = opts.kbDir !== undefined ? opts.kbDir : KB_DIR
  if (!dir) return { ok: false, status: 500, error: '找不到仓库内的 shangyi/kb' }
  const v = validateSlug(slug)
  if (!v.ok) return { ok: false, status: v.status, error: v.error }
  const abs = path.join(dir, v.bare + '.md')
  // 解析真实路径再比对，挡住符号链接逃逸
  let real
  try { real = fs.realpathSync(abs) } catch { return { ok: false, status: 404, error: '没有这篇文档' } }
  let realRoot
  try { realRoot = fs.realpathSync(dir) } catch { return { ok: false, status: 500, error: '知识库目录不可读' } }
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
    slug: slugOf(path.relative(dir, real)),
    title: fm.name || heading || path.basename(real, '.md'),
    markdown,
    group: fm.kind || null,
    id: fm.id || null,
    heading
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
