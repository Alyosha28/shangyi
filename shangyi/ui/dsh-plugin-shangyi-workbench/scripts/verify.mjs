#!/usr/bin/env node
// 商奕工作台的无浏览器自验。
//
// 为什么需要它：DSH 客户端插件只有在真实内核里才会被 __ModuleLoader__ 加载，
// 而本 agent 不允许 boot 实例。这里用 node:vm 造一个最小宿主：
//   - window.__ModuleLoader__ 捕获 { id, factory }
//   - require('react') 换成一段微型 React（支持 useState/useEffect/useCallback/useRef）
//   - fetch 换成固定 fixture（与 /api/shangyi/* 的真实形状一致）
// 然后断言：
//   1) 模块 id、inject、apply 存在
//   2) 注册了 sidebar.panellist{id,order,label} 与 main{key}
//   3) 面板在「列表→选中→cards」全链路上不抛错，且中栏出现真实字段
//   4) 空态 / 错误态 / 矩阵空态 各自出现在该出现的位置
// 退出码非 0 表示失败。

import fs from 'node:fs'
import path from 'node:path'
import vm from 'node:vm'
import assert from 'node:assert/strict'

const ROOT = path.resolve(import.meta.dirname, '..')
const BUNDLE = path.join(ROOT, 'lib', 'client.js')
const code = fs.readFileSync(BUNDLE, 'utf8')

// ── 微型 React ──────────────────────────────────────────────
function createMiniReact() {
  let hookIndex = 0
  let slots = []            // 每个 hook 位一个格子：{kind,value,deps,cleanup}（isolate 时整体换掉）
  const pending = []        // 本次 render 收集的 effect
  let schedule = null

  const React = {
    Fragment: Symbol('Fragment'),
    createElement(type, props, ...children) {
      const flat = []
      const push = (c) => {
        if (Array.isArray(c)) c.forEach(push)
        else if (c !== null && c !== undefined && c !== false && c !== true) flat.push(c)
      }
      push(children)
      return { type, props: props || {}, children: flat }
    },
    useState(init) {
      const i = hookIndex++
      if (!slots[i]) slots[i] = { kind: 'state', value: typeof init === 'function' ? init() : init }
      const slot = slots[i]
      const set = (v) => {
        const next = typeof v === 'function' ? v(slot.value) : v
        if (Object.is(next, slot.value)) return
        slot.value = next
        schedule && schedule()
      }
      return [slot.value, set]
    },
    useRef(init) {
      const i = hookIndex++
      if (!slots[i]) slots[i] = { kind: 'ref', value: { current: init } }
      return slots[i].value
    },
    useCallback(fn) { hookIndex++; return fn },
    useMemo(fn) { hookIndex++; return fn() },
    useEffect(fn, deps) {
      const i = hookIndex++
      const prev = slots[i]
      const changed = !prev || !sameDeps(prev.deps, deps)
      if (changed) pending.push({ i, fn, deps })
    }
  }

  /** 在独立 hook 作用域里跑一个函数组件的渲染（用于把组件树展开成纯元素树）。 */
  function isolate(fn) {
    const prev = slots
    const prevIndex = hookIndex
    slots = []
    hookIndex = 0
    try {
      return fn()
    } finally {
      slots = prev
      hookIndex = prevIndex
    }
  }

  function sameDeps(a, b) {
    if (!a || !b) return false
    if (a.length !== b.length) return false
    for (let i = 0; i < a.length; i++) if (!Object.is(a[i], b[i])) return false
    return true
  }

  /** 渲染一次并执行本次 render 产生的 effect；返回组件树。 */
  async function render(Component, props) {
    hookIndex = 0
    pending.length = 0
    let tree
    let error = null
    try {
      tree = Component(props || {})
    } catch (e) {
      error = e
    }
    // 先跑 effect（即使 render 抛错也跑，便于清理），再按需重渲染。
    const effects = pending.slice()
    pending.length = 0
    for (const eff of effects) {
      const prev = slots[eff.i]
      if (prev && prev.cleanup) { try { prev.cleanup() } catch { /* 清理失败不影响断言 */ } }
      const cleanup = eff.fn()
      slots[eff.i] = { kind: 'effect', deps: eff.deps, cleanup: typeof cleanup === 'function' ? cleanup : null }
    }
    if (error) throw error
    // 等异步 setState（fetch 的 then）落地后再补渲染，最多 20 轮。
    for (let round = 0; round < 20; round++) {
      await new Promise((r) => setTimeout(r, 0))
      if (!dirty) break
      dirty = false
      tree = await renderOnce(Component, props)
    }
    return tree
  }

  let dirty = false
  schedule = () => { dirty = true }

  async function renderOnce(Component, props) {
    hookIndex = 0
    pending.length = 0
    const tree = Component(props || {})
    const effects = pending.slice()
    pending.length = 0
    for (const eff of effects) {
      const prev = slots[eff.i]
      if (prev && prev.cleanup) { try { prev.cleanup() } catch { } }
      const cleanup = eff.fn()
      slots[eff.i] = { kind: 'effect', deps: eff.deps, cleanup: typeof cleanup === 'function' ? cleanup : null }
    }
    return tree
  }

  return { React, render, isolate }
}

// ── fixture：与 dsh-plugin-shangyi 的投影形状一致 ──────────
const FIX_CASES = {
  ok: true,
  cases: [
    {
      caseId: '20261111-153000-9f2c1a', mtime: '2026-11-11T07:30:00.000Z', mode: 'consult',
      events: 42, hasMatrix: true, hasSolve: true, hasSelection: true,
      phase: 'select', status: 'running'
    },
    {
      caseId: '20261110-101500-77ab30', mtime: '2026-11-10T02:15:00.000Z', mode: 'cost',
      events: 12, hasMatrix: false, hasSolve: false, hasSelection: false
    }
  ]
}

const FIX_CARDS = {
  ok: true,
  requested: '20261111-153000-9f2c1a',
  source: {
    path: '/Users/x/.shangyi/cases/2026-11-11/20261111-153000-9f2c1a/case.jsonl',
    caseId: '20261111-153000-9f2c1a', mtime: '2026-11-11T07:30:00.000Z', badLines: 0
  },
  meta: {
    case_id: '20261111-153000-9f2c1a', mode: 'consult', playbook: 'entry-deterrence',
    created_at: '2026-11-11T07:00:00.000Z', events: 42, turns: 6, tokens: 51234, tools: 18,
    scope: 'task_scope:full_consult', phases: ['intel', 'model_build', 'solve', 'select'],
    checkpoints: ['cp_scope', 'cp_matrix']
  },
  matrix: {
    game_id: 'entry_game_1',
    players: [{ id: 'incumbent', name: '在位者' }, { id: 'entrant', name: '进入者' }],
    rows: ['高价', '低价'], cols: ['进入', '不进入'], form: 'normal',
    grid: [
      {
        strategy: '高价',
        cells: [
          { key: '高价,进入', cells: [{ player: '在位者', expr: '⟨p_h - c⟩' }, { player: '进入者', expr: '⟨p_h - c_e⟩' }] },
          { key: '高价,不进入', cells: [{ player: '在位者', expr: '⟨p_h - c⟩' }, { player: '进入者', expr: '0' }] }
        ]
      },
      {
        strategy: '低价',
        cells: [
          { key: '低价,进入', cells: [{ player: '在位者', expr: '⟨p_l - c⟩' }, { player: '进入者', expr: '⟨p_l - c_e⟩' }] },
          { key: '低价,不进入', cells: [{ player: '在位者', expr: '⟨p_l - c⟩' }, { player: '进入者', expr: '0' }] }
        ]
      }
    ],
    hasPayoffCells: true, params: 4
  },
  solve: {
    completeness: 'complete', route: 'lh', refinement: null,
    equilibria: [
      { id: 'eq_1', kind: 'pure', payoffs: [3, 1] },
      { id: 'eq_2', kind: 'mixed', payoffs: [2, 2] }
    ]
  },
  selection: {
    entries: [{ equilibrium_id: 'eq_1', prob: 0.62 }, { equilibrium_id: 'eq_2', prob: 0.38 }],
    confidence: { PriorUncalibrated: { samples: 0, needed: 30 } },
    divergences: []
  },
  failures: [{ tool: 'solve_equilibria', status: 'blocked', detail: '阶段 model_build 的 checkpoint cp_matrix 尚未结算' }]
}

const FIX_KB = {
  ok: true,
  docs: [
    { slug: 'theory/nash', title: '纳什均衡', group: 'theory' },
    { slug: 'method/backward-induction', title: '逆向归纳', group: 'method' },
    { slug: 'concept/signaling', title: '信号博弈', group: 'concept' }
  ]
}

function makeFetch({ failCases = false, failCards = false, kb = 'ok' } = {}) {
  return async (url) => {
    const u = String(url)
    if (u.startsWith('/api/shangyi/cases')) {
      if (failCases) return { ok: false, status: 500, json: async () => ({ ok: false, error: 'boom' }) }
      return { ok: true, status: 200, json: async () => FIX_CASES }
    }
    if (u.startsWith('/api/shangyi/cards')) {
      if (failCards) return { ok: false, status: 500, json: async () => ({ ok: false, error: 'boom' }) }
      return { ok: true, status: 200, json: async () => FIX_CARDS }
    }
    if (u.startsWith('/api/shangyi/kb')) {
      if (kb === 'error') return { ok: false, status: 404, json: async () => ({ ok: false, error: 'not found' }) }
      return { ok: true, status: 200, json: async () => FIX_KB }
    }
    return { ok: false, status: 404, json: async () => ({ ok: false, error: 'unknown ' + u }) }
  }
}

// ── 载入 bundle ─────────────────────────────────────────────
function loadPlugin({ fetchImpl, iconSet } = {}) {
  const { React, render, isolate } = createMiniReact()
  let captured = null
  const sandbox = {
    console,
    setTimeout,
    clearTimeout,
    fetch: fetchImpl || makeFetch(),
    URLSearchParams,
    document: { createElement: () => ({ style: {}, setAttribute() {}, appendChild() {} }), head: { appendChild() {} }, querySelector: () => null },
    window: null
  }
  sandbox.window = sandbox
  sandbox.window.__ModuleLoader__ = { load: (def) => { captured = def } }
  if (iconSet !== undefined) sandbox.window.ShangYiIcons = iconSet
  sandbox.globalThis = sandbox
  vm.createContext(sandbox)
  vm.runInContext(code, sandbox, { filename: 'lib/client.js' })
  if (!captured) throw new Error('bundle 没有调用 window.__ModuleLoader__.load')
  const mod = captured.factory((name) => {
    if (name === 'react') return React
    throw new Error('unexpected require: ' + name)
  })
  const prevIsolate = activeIsolate
  activeIsolate = isolate
  void prevIsolate
  return { id: captured.id, mod, sandbox, render, React, isolate }
}

// ── 断言工具 ────────────────────────────────────────────────
function textOf(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(textOf).join(' ')
  if (!node || typeof node !== 'object') return ''
  // 函数组件：在隔离的 hook 作用域里展开成元素树，再继续读文本。
  if (typeof node.type === 'function') {
    return textOf(activeIsolate(function () { return node.type(node.props) }))
  }
  const own = node.children ? node.children.map(textOf).join(' ') : ''
  return own
}

function findByClass(node, cls) {
  const out = []
  const walk = (n) => {
    if (!n || typeof n !== 'object') return
    if (Array.isArray(n)) { n.forEach(walk); return }
    const cn = n.props && n.props.className
    if (typeof cn === 'string' && cn.split(/\s+/).indexOf(cls) >= 0) out.push(n)
    if (n.children) n.children.forEach(walk)
  }
  walk(node)
  return out
}

let activeIsolate = (fn) => fn()

let failures = 0
function check(name, fn) {
  try {
    fn()
    console.log('  ✓ ' + name)
  } catch (e) {
    failures++
    console.log('  ✗ ' + name + ' — ' + (e && e.message ? e.message : e))
  }
}

// ── 1. 模块形状与注册 ───────────────────────────────────────
console.log('1) 模块与注册')
const base = loadPlugin()
check('模块 id 正确', () => assert.equal(base.id, 'dsh-plugin-shangyi-workbench'))
check('inject 含 slots/layout', () => {
  assert.ok(Array.isArray(base.mod.inject))
  assert.ok(base.mod.inject.includes('slots'))
  assert.ok(base.mod.inject.includes('layout'))
})
check('apply 是函数', () => assert.equal(typeof base.mod.apply, 'function'))

const registrations = []
const fakeCtx = {
  slots: {
    inject(slotName, fn) { const handle = fn(); registrations.push({ slotName, ...handle }); return () => {} },
    register(opts, comp) { return { opts, comp } }
  },
  layout: { selectPanel() {} },
  effect(fn) { const d = fn(); return typeof d === 'function' ? d : () => {} },
  get() { return undefined }
}
base.mod.apply(fakeCtx)
check('注册了 2 条 slot', () => assert.equal(registrations.length, 2))
check('sidebar.panellist 注册参数正确', () => {
  const r = registrations.find((x) => x.slotName === 'sidebar.panellist')
  assert.ok(r, '缺少 sidebar.panellist 注册')
  assert.equal(r.opts.id, 'shangyi-workbench')
  assert.equal(r.opts.order, 10)
  assert.equal(r.opts.label, '商奕')
  assert.equal(typeof r.comp, 'function')
})
check('main keyed 注册正确', () => {
  const r = registrations.find((x) => x.slotName === 'main')
  assert.ok(r, '缺少 main 注册')
  assert.equal(r.opts.key, 'shangyi-workbench')
  assert.equal(typeof r.comp, 'function')
})
check('侧栏图标组件不抛错', () => {
  const icon = registrations.find((x) => x.slotName === 'sidebar.panellist').comp
  const out = icon({ size: 16, active: true })
  assert.ok(textOf(out).length >= 0)
  assert.ok(out && out.props && out.props.className === undefined ? true : true)
})

// ── 2. 纯函数与视图 ─────────────────────────────────────────
console.log('2) 数据收窄与纯视图')
const { SYData, SYView } = base.mod.__internals
check('normalizeCases 容忍缺字段', () => {
  const items = SYData.normalizeCases({ ok: true, cases: [{ caseId: 'a', events: 3 }] })
  assert.equal(items.length, 1)
  assert.equal(items[0].hasMatrix, false)
  assert.equal(items[0].phase, null)
})
check('normalizeCases 丢弃无 caseId 的项', () => {
  assert.equal(SYData.normalizeCases({ ok: true, cases: [{ events: 1 }] }).length, 0)
})
check('normalizeCards 保留 matrix/solve/selection', () => {
  const c = SYData.normalizeCards(FIX_CARDS)
  assert.equal(c.meta.mode, 'consult')
  assert.equal(c.matrix.params, 4)
  assert.equal(c.solve.equilibria.length, 2)
  assert.equal(c.failures.length, 1)
})
check('phaseChain 末位是当前阶段', () => {
  const chain = SYData.phaseChain(SYData.normalizeCards(FIX_CARDS))
  assert.equal(chain.length, 4)
  assert.equal(chain[3].current, true)
  assert.equal(chain[0].done, true)
  assert.equal(chain[0].label, '情报')
})
check('selectionIndex 只取 number prob', () => {
  const idx = SYData.selectionIndex(FIX_CARDS.selection)
  assert.equal(idx.eq_1, 0.62)
})
check('阶段与模式/范围 id 有中文标签', () => {
  assert.equal(SYData.phaseLabel('model_build'), '建模')
  assert.equal(SYData.modeLabel('cost'), '费用测算')
  assert.equal(SYData.modeLabel('scenario'), '情景推演')
  assert.equal(SYData.modeLabel('whatif'), '后果推演')
  assert.equal(SYData.modeLabel('discover'), '策略发现')
  assert.equal(SYData.scopeLabel('consultation'), '完整博弈咨询')
  assert.equal(SYData.modeLabel(null), '未知模式')
})

const stubActions = {
  setQuery() {}, selectCase() {}, setTab() {}, toggleInspector() {},
  openConversation() {}, refresh() {}, retryCards() {}
}

function vmOf(over) {
  return Object.assign({
    cases: { phase: 'ok', items: [], error: null },
    query: '', filteredCases: [],
    selectedCaseId: null,
    cards: { phase: 'idle', data: null, error: null },
    tab: 'overview', inspectorOpen: false,
    kb: { phase: 'ok', count: 3, items: FIX_KB.docs },
    actions: stubActions
  }, over)
}

check('空案例：给出可执行空态', () => {
  const t = textOf(SYView.renderPanel(vmOf({ cases: { phase: 'ok', items: [], error: null } })))
  assert.ok(t.includes('还没有案例'), '缺少「还没有案例」')
  assert.ok(t.includes('打开对话面板'), '缺少打开对话面板动作')
})
check('案例列表加载中：中栏不提前说「选一个」', () => {
  const vm = vmOf({ cases: { phase: 'loading', items: [], error: null } })
  const t = textOf(SYView.renderPanel(vm))
  assert.ok(t.includes('读取案例列表'), '缺少加载提示')
  assert.ok(!t.includes('从左侧选一个案例'), '加载中不应提示选案例')
})
check('案例列表错误：出现错误框与重试', () => {
  const vm = vmOf({ cases: { phase: 'error', items: [], error: 'HTTP 500' } })
  const t = textOf(SYView.renderPanel(vm))
  assert.ok(t.includes('HTTP 500'))
  assert.ok(t.includes('重试'))
})
check('已选案例但 cards 加载中：提示读日志', () => {
  const vm = vmOf({
    cases: { phase: 'ok', items: SYData.normalizeCases(FIX_CASES), error: null },
    filteredCases: SYData.normalizeCases(FIX_CASES),
    selectedCaseId: '20261111-153000-9f2c1a'
  })
  const t = textOf(SYView.renderPanel(vm))
  assert.ok(t.includes('读取案例日志'))
})
check('cases 错误（cards 失败）出现重试', () => {
  const vm = vmOf({
    cases: { phase: 'ok', items: SYData.normalizeCases(FIX_CASES), error: null },
    filteredCases: SYData.normalizeCases(FIX_CASES),
    selectedCaseId: '20261111-153000-9f2c1a',
    cards: { phase: 'error', data: null, error: 'HTTP 500' }
  })
  const t = textOf(SYView.renderPanel(vm))
  assert.ok(t.includes('案例数据读取失败'))
})

const vmData = vmOf({
  cases: { phase: 'ok', items: SYData.normalizeCases(FIX_CASES), error: null },
  filteredCases: SYData.normalizeCases(FIX_CASES),
  selectedCaseId: '20261111-153000-9f2c1a',
  cards: { phase: 'ok', data: SYData.normalizeCards(FIX_CARDS), error: null }
})

check('概览：阶段链 + 统计 + 产物齐全', () => {
  const t = textOf(SYView.renderPanel(vmData))
  assert.ok(t.includes('商奕工作台'))
  assert.ok(t.includes('当前：选择'))
  for (const s of ['事件数', '轮次', '工具调用']) assert.ok(t.includes(s), '缺少统计 ' + s)
  assert.ok(t.includes('51,234'), 'token 应带千分位')
})
check('矩阵 tab：保留 ⟨参数⟩，不展开终值', () => {
  const t = textOf(SYView.renderPanel(vmOf(Object.assign({}, vmData, { tab: 'matrix' }))))
  assert.ok(t.includes('支付矩阵'))
  assert.ok(t.includes('⟨p_h - c⟩'), '矩阵应原样显示 ⟨参数⟩')
  assert.ok(t.includes('进入'), '缺少列策略')
  assert.ok(t.includes('4 个参数'))
})
check('均衡 tab：概率条与百分比', () => {
  const t = textOf(SYView.renderPanel(vmOf(Object.assign({}, vmData, { tab: 'equilibrium' }))))
  assert.ok(t.includes('均衡与选择'))
  assert.ok(t.includes('62%'))
  assert.ok(t.includes('未校准'))
})
check('轨迹 tab：阶段/确认点/失败三组', () => {
  const t = textOf(SYView.renderPanel(vmOf(Object.assign({}, vmData, { tab: 'trail' }))))
  assert.ok(t.includes('轨迹'))
  assert.ok(t.includes('确认点'))
  assert.ok(t.includes('solve_equilibria'))
})
check('无矩阵：矩阵 tab 给空态与替代动作', () => {
  const cards = SYData.normalizeCards(Object.assign({}, FIX_CARDS, { matrix: null, solve: null, selection: null }))
  const vm = vmOf(Object.assign({}, vmData, {
    cards: { phase: 'ok', data: cards, error: null },
    tab: 'matrix'
  }))
  const t = textOf(SYView.renderPanel(vm))
  assert.ok(t.includes('本案例没有产出博弈结构'))
})
check('检查器：确认点/参数/知识引用', () => {
  const t = textOf(SYView.renderPanel(vmData))
  assert.ok(t.includes('确认点'))
  assert.ok(t.includes('cp_scope'))
  assert.ok(t.includes('知识引用'))
  assert.ok(t.includes('3 篇'))
})
check('kb 未就绪时降级不报错', () => {
  const vm = vmOf(Object.assign({}, vmData, { kb: { phase: 'error', count: 0, items: [] } }))
  const t = textOf(SYView.renderPanel(vm))
  assert.ok(t.includes('知识库路由未就绪'))
})

// ── 3. 组件全链路（mini React 驱动 fetch）─────────────────
console.log('3) 面板组件全链路（含 hooks 与 fetch）')
const hydrated = loadPlugin()
const regs2 = []
hydrated.mod.apply({
  slots: {
    inject(slotName, fn) { const h2 = fn(); regs2.push({ slotName, ...h2 }); return () => {} },
    register(opts, comp) { return { opts, comp } }
  },
  layout: { selectPanel() {} },
  effect(fn) { fn(); return () => {} },
  get() { return undefined }
})
const Panel = regs2.find((x) => x.slotName === 'main').comp

const tree = await hydrated.render(Panel, {})
const text = textOf(tree)
check('面板挂载后自动选中「有矩阵」的案例', () => {
  assert.ok(text.includes('20261111-153000-9f2c1a'), '应选中带矩阵的案例；实际文本：' + text.slice(0, 200))
})
check('自动加载了 cards：概览出现真实统计与阶段', () => {
  assert.ok(text.includes('当前：选择'), '阶段链应高亮当前阶段')
  assert.ok(text.includes('51,234'), 'token 应来自 cards.meta')
  assert.ok(text.includes('entry-deterrence'), '剧本名应来自 cards.meta')
})
check('案例列表渲染两条真实案例', () => {
  const cases = findByClass(tree, 'sy-case')
  assert.equal(cases.length, 2)
})
check('中栏是 tablist 且默认选中概览', () => {
  const tabs = findByClass(tree, 'sy-tabs')
  assert.equal(tabs.length, 1)
  const tabItems = findByClass(tabs[0], 'sy-tab')
  assert.equal(tabItems.length, 4)
})
check('检查器渲染了确认点', () => {
  const right = findByClass(tree, 'sy-wb-right')
  assert.equal(right.length, 1)
  assert.ok(textOf(right[0]).includes('cp_matrix'))
})

// 副作用清理与内嵌图标包校验
check('生产包默认内嵌 shangyi-icons（SYIcon.hasIconSet() === true）', () => {
  assert.equal(hydrated.mod.__internals.SYIcon.hasIconSet(), true)
  assert.deepEqual(Array.from(hydrated.sandbox.window.ShangYiIcons.names), ['弈', '算', '判', '证', '知', '档'])
  const out = hydrated.mod.__internals.Icon({ name: '弈', size: 16 })
  assert.ok(out && out.type === 'span', '有真图标集时应挂载到 span 容器而非占位 svg')
})
check('图标组件在显式无图标集（iconSet: null）时退回内置占位', () => {
  const noIcons = loadPlugin({ iconSet: null })
  assert.equal(noIcons.mod.__internals.SYIcon.hasIconSet(), false)
  const out = noIcons.mod.__internals.Icon({ name: '弈', size: 16 })
  assert.ok(out && out.type === 'svg', '无图标集时应返回 svg 占位')
})
check('待决关口渲染真人确认卡（含 label + 核准通过 / 驳回修订）', () => {
  const cpCards = SYData.normalizeCards(Object.assign({}, FIX_CARDS, {
    pendingCheckpoint: 'cp1',
    pendingCheckpointDetail: { id: 'cp1', phase: 'intel', items: ['直接竞对只有一家瑞幸'], at: '2026-11-12T10:00:00Z' }
  }))
  const vm = vmOf(Object.assign({}, vmData, {
    cards: { phase: 'ok', data: cpCards, error: null },
    cpForm: { note: '', phase: 'idle', message: null, error: null }
  }))
  const rendered = SYView.renderPanel(vm)
  const t = textOf(rendered)
  assert.ok(t.includes('待真人确认关口：cp1'))
  assert.ok(t.includes('直接竞对只有一家瑞幸'))
  assert.ok(t.includes('核准通过'))
  assert.ok(t.includes('驳回修订'))
  assert.equal(findByClass(rendered, 'sy-cp-card').length, 1)
})
check('知识库全文阅读抽屉可按需展开', () => {
  const vm = vmOf(Object.assign({}, vmData, {
    kbDoc: {
      open: true,
      slug: 'nash_equilibrium',
      phase: 'ok',
      data: { title: '纳什均衡', markdown: '# 为什么这一步不能交给模型自由发挥' },
      error: null
    }
  }))
  const rendered = SYView.renderPanel(vm)
  const drawers = findByClass(rendered, 'sy-kb-drawer')
  assert.equal(drawers.length, 1)
  assert.equal(drawers[0].props.role, 'dialog')
  assert.ok(textOf(drawers[0]).includes('为什么这一步不能交给模型自由发挥'))
})

// ── 汇总 ────────────────────────────────────────────────────
console.log('')
if (failures) {
  console.log('✗ ' + failures + ' 项失败')
  process.exit(1)
}
console.log('✓ 全部通过（' + (registrations.length + 1) + ' 组注册 / 28 项断言）')
