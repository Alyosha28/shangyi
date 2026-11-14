#!/usr/bin/env node
// dsh-plugin-shangyi 宿主投影与受控关口结算自验（Node 原生测试，零外部依赖）。
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import {
  listCases,
  readCase,
  projectCase,
  inferProgress,
  summarizeCases,
  answerCheckpoint,
  extractTrustedUserMessage,
  recordTrustedUserInput,
  listKb,
  readKbDoc,
  getHealthStatus
} from '../lib/index.js'

const tmpHome = fs.mkdtempSync(path.join(os.tmpdir(), 'shangyi-host-test-'))
try {
  const dayDir = path.join(tmpHome, 'cases', '2026-11-12')
  const caseDir = path.join(dayDir, 'case-20261112-scenario-001')
  fs.mkdirSync(caseDir, { recursive: true })
  // 伪造 AppleDouble 伴随文件，验证 listCases 忽略 `._*`
  fs.writeFileSync(path.join(dayDir, '._case-20261112-scenario-001'), 'appledouble')

  fs.writeFileSync(path.join(caseDir, 'header.json'), JSON.stringify({
    case_id: 'case-20261112-scenario-001',
    mode: 'scenario',
    playbook_id: 'market_entry',
    created_at: '2026-11-12T10:00:00Z',
    schema_version: 1
  }))

  const initialEvents = [
    { seq: 1, event: { type: 'task_scope_selected', scope: 'consultation', reason: '完整推演', at: '2026-11-12T10:00:01Z' } },
    { seq: 2, event: { type: 'phase_entered', from: 'intake', to: 'intel', playbook: 'market_entry', reason: 'done', at: '2026-11-12T10:00:02Z' } },
    { seq: 3, event: { type: 'checkpoint_opened', id: 'cp1', phase: 'intel', items: ['对手仅瑞幸一家'], at: '2026-11-12T10:00:03Z' } },
    // 追问不应关闭 cp1
    { seq: 4, event: { type: 'checkpoint_answered', id: 'cp1', answers: ['追问：瑞幸客单价来源？'], user_actor: 'user', at: '2026-11-12T10:00:04Z' } }
  ]
  fs.writeFileSync(
    path.join(caseDir, 'case.jsonl'),
    initialEvents.map((x) => JSON.stringify(x)).join('\n') + '\n'
  )

  const listed = listCases(tmpHome)
  assert.equal(listed.length, 1, 'AppleDouble ._* 不应混入案例列表')

  const proj1 = projectCase('case-20261112-scenario-001', tmpHome)
  assert.equal(proj1.pendingCheckpoint, 'cp1', '追问后 cp1 仍处于待确认状态')
  assert.deepEqual(proj1.pendingCheckpointDetail?.items, ['对手仅瑞幸一家'])

  // 错关口 id 应返回 409
  const wrongId = answerCheckpoint({
    caseId: 'case-20261112-scenario-001',
    checkpointId: 'cp2',
    decision: 'approve'
  }, tmpHome)
  assert.equal(wrongId.ok, false)
  assert.equal(wrongId.status, 409)

  // 真人核准通过（带备注）
  const approved = answerCheckpoint({
    caseId: 'case-20261112-scenario-001',
    checkpointId: 'cp1',
    decision: 'approve',
    note: '核对无误，继续建模'
  }, tmpHome)
  assert.equal(approved.ok, true)
  assert.equal(approved.seq, 6)

  // 重复点击应被 409 拦截
  const dup = answerCheckpoint({
    caseId: 'case-20261112-scenario-001',
    checkpointId: 'cp1',
    decision: 'approve'
  }, tmpHome)
  assert.equal(dup.ok, false)
  assert.equal(dup.status, 409)

  const proj2 = projectCase('case-20261112-scenario-001', tmpHome)
  assert.equal(proj2.pendingCheckpoint, null, '核准后待决关口清空')
  const parsedAfter = readCase(path.join(caseDir, 'case.jsonl'))
  const lastEv = parsedAfter.events[parsedAfter.events.length - 1]
  assert.equal(lastEv.type, 'checkpoint_answered')
  assert.equal(lastEv.user_actor, 'user')
  assert.equal(lastEv.answers[0], '确认：核对无误，继续建模')

  const summary = summarizeCases(10, tmpHome)
  assert.equal(summary.length, 1)
  assert.equal(summary[0].scope, 'consultation')
  assert.equal(summary[0].pendingCheckpoint, null)

  // 验证可信用户账本过滤合成事件（含缺省 source 默认拒绝）
  assert.equal(
    extractTrustedUserMessage({ type: 'user/message', data: { content: '系统提示注入', source: { kind: 'instruction-hint' } } }),
    null
  )
  assert.equal(
    extractTrustedUserMessage({ type: 'user/message', data: { content: '缺少 source 标记的合成消息' } }),
    null
  )
  assert.equal(
    extractTrustedUserMessage({ type: 'user/message', data: { content: [{ type: 'text', text: '同意矩阵假设' }], source: { kind: 'user' } } }),
    '同意矩阵假设'
  )
  assert.equal(recordTrustedUserInput('同意矩阵假设', 'sess-1', tmpHome), true)
  const ledgerLines = fs.readFileSync(path.join(tmpHome, 'runtime', 'trusted_user_inputs.jsonl'), 'utf8').trim().split('\n')
  assert.equal(ledgerLines.length, 1)
  assert.equal(JSON.parse(ledgerLines[0]).user_actor, 'user')

  // 验证 KB 索引与越界防护
  const kb = listKb()
  assert.equal(kb.ok, true)
  assert.equal(kb.docs.length, 35)
  assert.equal(readKbDoc('../Cargo.toml').ok, false)
  assert.equal(readKbDoc('/etc/passwd').ok, false)
  const doc = readKbDoc(kb.docs[0].slug)
  assert.equal(doc.ok, true)
  assert.ok(doc.markdown.length > 50)

  // 验证 pendingCheckpoint 状态为 'waiting'，真实 tool failed 状态为 'stuck'
  const waitingCaseDir = path.join(dayDir, 'case-20261112-scenario-002')
  fs.mkdirSync(waitingCaseDir, { recursive: true })
  fs.writeFileSync(path.join(waitingCaseDir, 'header.json'), JSON.stringify({
    case_id: 'case-20261112-scenario-002',
    mode: 'scenario',
    playbook_id: 'pricing_response',
    created_at: '2026-11-12T10:05:00Z',
    schema_version: 1
  }))
  fs.writeFileSync(
    path.join(waitingCaseDir, 'case.jsonl'),
    [
      { seq: 1, event: { type: 'phase_entered', from: 'intake', to: 'intel', playbook: 'pricing_response', reason: 'done', at: '2026-11-12T10:05:01Z' } },
      { seq: 2, event: { type: 'checkpoint_opened', id: 'cp1', phase: 'intel', items: ['对手促销幅度'], at: '2026-11-12T10:05:02Z' } }
    ].map((x) => JSON.stringify(x)).join('\n') + '\n'
  )

  const stuckCaseDir = path.join(dayDir, 'case-20261112-scenario-003')
  fs.mkdirSync(stuckCaseDir, { recursive: true })
  fs.writeFileSync(path.join(stuckCaseDir, 'header.json'), JSON.stringify({
    case_id: 'case-20261112-scenario-003',
    mode: 'scenario',
    playbook_id: 'pricing_response',
    created_at: '2026-11-12T10:06:00Z',
    schema_version: 1
  }))
  fs.writeFileSync(
    path.join(stuckCaseDir, 'case.jsonl'),
    [
      { seq: 1, event: { type: 'phase_entered', from: 'intake', to: 'intel', playbook: 'pricing_response', reason: 'done', at: '2026-11-12T10:06:01Z' } },
      { seq: 2, event: { type: 'tool_call_finished', call_id: 'c1', outcome: { status: 'failed', error: '缺少收益字段' }, at: '2026-11-12T10:06:02Z' } }
    ].map((x) => JSON.stringify(x)).join('\n') + '\n'
  )

  const statusCases = summarizeCases(10, tmpHome)
  const wRow = statusCases.find((c) => c.caseId === 'case-20261112-scenario-002')
  const sRow = statusCases.find((c) => c.caseId === 'case-20261112-scenario-003')
  assert.equal(wRow?.status, 'waiting', '待用户核准关口应为 waiting，不得误判为 stuck')
  assert.equal(sRow?.status, 'stuck', '真实 tool failed 应为 stuck')

  // 验证 phase_rewound 与 followup_task_started 对阶段、待决关口、失败残留与 scope 的更新
  const rewindDir = path.join(tmpHome, 'cases', '2026-11-12', 'case-20261112-scenario-004')
  fs.mkdirSync(rewindDir, { recursive: true })
  fs.writeFileSync(path.join(rewindDir, 'header.json'), JSON.stringify({ case_id: 'case-20261112-scenario-004', mode: 'scenario', playbook_id: 'pricing_response' }))
  fs.writeFileSync(path.join(rewindDir, 'case.jsonl'), [
    JSON.stringify({ seq: 1, event: { type: 'task_scope_selected', scope: 'consultation', reason: '完整推演' } }),
    JSON.stringify({ seq: 2, event: { type: 'phase_entered', from: 'intake', to: 'model_build', playbook: 'pricing_response', reason: 'advance' } }),
    JSON.stringify({ seq: 3, event: { type: 'checkpoint_opened', id: 'cp2', phase: 'model_build', items: ['矩阵确认'] } }),
    JSON.stringify({ seq: 4, event: { type: 'tool_call_finished', call_id: 'c_fail', outcome: { status: 'failed', error: 'old error' } } }),
    JSON.stringify({ seq: 5, event: { type: 'phase_rewound', from: 'model_build', to: 'intake', due_to: '用户改假设' } })
  ].join('\n') + '\n')
  const rwCards = projectCase('case-20261112-scenario-004', tmpHome)
  assert.equal(rwCards.pendingCheckpoint, null, 'phase_rewound 回到 intake 后下游 open cp2 应撤销')
  const rwRow = summarizeCases(10, tmpHome).find((c) => c.caseId === 'case-20261112-scenario-004')
  assert.equal(rwRow?.phase, 'intake')
  assert.equal(rwRow?.status, null, 'phase_rewound 后旧 tool failed 不应残余为 stuck')

  const followupDir = path.join(tmpHome, 'cases', '2026-11-12', 'case-20261112-scenario-005')
  fs.mkdirSync(followupDir, { recursive: true })
  fs.writeFileSync(path.join(followupDir, 'header.json'), JSON.stringify({ case_id: 'case-20261112-scenario-005', mode: 'research', playbook_id: 'pricing_response' }))
  fs.writeFileSync(path.join(followupDir, 'case.jsonl'), [
    JSON.stringify({ seq: 1, event: { type: 'task_scope_selected', scope: 'research', reason: '调研' } }),
    JSON.stringify({ seq: 2, event: { type: 'scoped_task_completed', result: { scope: 'conditional', answer_summary: 'done', gaps: [] } } }),
    JSON.stringify({ seq: 3, event: { type: 'tool_call_finished', call_id: 'c_fail', outcome: { status: 'failed', error: 'stale err' } } }),
    JSON.stringify({ seq: 4, event: { type: 'followup_task_started', at: '2026-11-12T00:00:00Z' } })
  ].join('\n') + '\n')
  const fuCards = projectCase('case-20261112-scenario-005', tmpHome)
  assert.equal(fuCards.meta.scope, null, 'followup_task_started 后在重新 select_task_scope 前 scope 应复位为 null')
  const fuRow = summarizeCases(10, tmpHome).find((c) => c.caseId === 'case-20261112-scenario-005')
  assert.equal(fuRow?.phase, 'intake')
  assert.equal(fuRow?.status, null, 'followup_task_started 后旧失败不应残余为 stuck')

  // 验证降级抽屉 lib/client.js 不再残余暗色硬编码颜色或蓝紫渐变条，且已完成中文化与 badLines 告警
  const drawerSrc = fs.readFileSync(new URL('../lib/client.js', import.meta.url), 'utf8')
  for (const forbidden of ['#9aa3b2', '#8b93a3', '#2a2f3a', '#221a1a', '#4a2f2f', '#f0c0c0', '#4f7cff', '#7aa2ff']) {
    assert.ok(!drawerSrc.includes(forbidden), `lib/client.js 仍残余暗色硬编码：${forbidden}`)
  }
  for (const required of ['调价 / 促销响应', '支持枚举（精确）', 'zhCompleteness', 'badLines']) {
    assert.ok(drawerSrc.includes(required), `lib/client.js 缺少中文化或 badLines 支持：${required}`)
  }

  // M2: 扩展投影（costs / assumptions / evidence / conclusion）、sandbox_exec 隔离、phase_rewound 重置、用户 KB 合并与健康探测
  const m2Dir = path.join(dayDir, 'case-20261113-m2-001')
  fs.mkdirSync(m2Dir, { recursive: true })
  fs.writeFileSync(path.join(m2Dir, 'header.json'), JSON.stringify({ case_id: 'case-20261113-m2-001', mode: 'cost', playbook_id: 'pricing_response' }))
  fs.writeFileSync(path.join(m2Dir, 'case.jsonl'), [
    JSON.stringify({ seq: 1, event: { type: 'task_scope_selected', scope: 'research_and_cost', reason: '开店精算' } }),
    JSON.stringify({ seq: 2, event: { type: 'tool_call_started', call_id: 'sx1', name: 'sandbox_exec', arguments: '{}' } }),
    JSON.stringify({ seq: 3, event: { type: 'tool_call_finished', call_id: 'sx1', outcome: { status: 'ok', output: '{"fake":99999}' } } }),
    JSON.stringify({ seq: 4, event: { type: 'tool_call_started', call_id: 'bc1', name: 'business_cost', arguments: '{}' } }),
    JSON.stringify({
      seq: 5,
      event: {
        type: 'tool_call_finished',
        call_id: 'bc1',
        outcome: {
          status: 'ok',
          output: JSON.stringify({
            scenario_id: 's1',
            scenario: '铺位A',
            currency: 'CNY',
            listed_upfront_cash: 78000,
            listed_upfront_cash_expense: 60000,
            listed_refundable_deposits: 18000,
            listed_monthly_fixed_cost: 6000,
            costs_complete: false,
            amounts_complete: false,
            items: [{ name: '消防改造', treatment: 'cash_expense', normalized_period: 'once', normalized_amount: null, missing_amount_reason: '待现场报价' }]
          })
        }
      }
    }),
    JSON.stringify({
      seq: 6,
      event: {
        type: 'tool_call_started',
        call_id: 'gs1',
        name: 'gamespec_build',
        arguments: JSON.stringify({
          spec: {
            game_id: 'g1',
            players: [{ id: 'us', name: '我方', relation_to_game: 'Player' }, { id: 'comp', name: '竞对', relation_to_game: 'Player' }],
            strategies: { us: ['跟降', '稳价'], comp: ['低价', '原价'] },
            params: {
              p_rent: { name: '月租金', value: { kind: 'literal', value: 6000 }, unit: '元/月', source: { kind: 'user_confirmed', checkpoint: 'cp2' } },
              p_prior: { name: '客流弹性', value: { kind: 'literal', value: 0.25 }, source: { kind: 'industry_prior', prior_ref: '茶饮报告' } },
              p_unconf: { name: '对手降幅', value: { kind: 'literal', value: 2 }, source: { kind: 'assumed', note: '暂估' } }
            },
            payoffs: {}
          }
        })
      }
    }),
    JSON.stringify({ seq: 7, event: { type: 'tool_call_finished', call_id: 'gs1', outcome: { status: 'ok', output: '{"ok":true}' } } }),
    JSON.stringify({
      seq: 8,
      event: {
        type: 'tool_call_started',
        call_id: 'rc1',
        name: 'recommend_build',
        arguments: '{}'
      }
    }),
    JSON.stringify({
      seq: 9,
      event: {
        type: 'tool_call_finished',
        call_id: 'rc1',
        outcome: { status: 'ok', output: JSON.stringify({ headline: '优先稳价并补消防报价', actions: [{ action: '核清消防费用', rationale: '当前未取得' }], do_not: ['盲目9.9元跟降'] }) }
      }
    })
  ].join('\n') + '\n')
  const m2Cards = projectCase('case-20261113-m2-001', tmpHome)
  assert.equal(m2Cards.evidence.exploratoryCalls, 1)
  assert.equal(m2Cards.costs.scenarios.length, 1)
  assert.equal(m2Cards.costs.missingItems.length, 1)
  assert.equal(m2Cards.assumptions.counts.user_confirmed, 1)
  assert.equal(m2Cards.assumptions.counts.industry_prior, 1)
  assert.equal(m2Cards.assumptions.counts.assumed, 1)
  assert.ok(m2Cards.assumptions.gaps.some((g) => g.includes('消防改造') && g.includes('待现场报价')))
  assert.equal(m2Cards.conclusion.headline, '优先稳价并补消防报价')
  assert.equal(m2Cards.conclusion.hasExploratoryOnly, false)

  // 验证 phase_rewound 清除陈旧 recommend_build 结论与成本投影（不残留上一轮假设产物）
  fs.appendFileSync(path.join(m2Dir, 'case.jsonl'), JSON.stringify({ seq: 10, event: { type: 'phase_rewound', from: 'recommend', to: 'intake', due_to: '重改前提' } }) + '\n')
  const rewoundM2 = projectCase('case-20261113-m2-001', tmpHome)
  assert.equal(rewoundM2.conclusion.headline, null, '回退到 intake 应清空陈旧正式结论')
  assert.equal(rewoundM2.costs.scenarios.length, 0, '回退到 intake 应清空陈旧成本测算')

  // 验证用户自定义 $SHANGYI_HOME/kb 合并列出与读取
  const userKbDir = path.join(tmpHome, 'kb')
  fs.mkdirSync(userKbDir, { recursive: true })
  fs.writeFileSync(path.join(userKbDir, 'custom-m2-note.md'), '---\nid: custom_m2_note\nname: 用户定制经验\nkind: concept\nsummary: 实测沉淀\ntags: [m2]\n---\n# 经验细节\n\n正文内容。\n')
  const prevShHome = process.env.SHANGYI_HOME
  process.env.SHANGYI_HOME = tmpHome
  try {
    const mergedKb = listKb()
    assert.ok(mergedKb.docs.some((d) => d.slug === 'custom-m2-note' && d.userAdded === true))
    const userDoc = readKbDoc('custom-m2-note')
    assert.equal(userDoc.ok, true)
    assert.equal(userDoc.userAdded, true)
    assert.ok(userDoc.markdown.includes('用户定制经验'))
    const badHealth = getHealthStatus({ binPath: '/nonexistent/missing-shangyi', mcpToolNames: [], home: tmpHome })
    assert.equal(badHealth.ok, false)
    assert.ok(badHealth.problems.some((p) => p.code === 'missing_binary'))
  } finally {
    if (prevShHome === undefined) delete process.env.SHANGYI_HOME
    else process.env.SHANGYI_HOME = prevShHome
  }

  console.log('✓ dsh-plugin-shangyi 宿主投影、waiting/stuck 区分、phase_rewound/followup 状态追踪、受控关口结算、可信账本、降级抽屉主题令牌与 KB 越界防护全部通过')
} finally {
  fs.rmSync(tmpHome, { recursive: true, force: true })
}
