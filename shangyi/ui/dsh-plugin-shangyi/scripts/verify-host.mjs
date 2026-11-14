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
  assert.equal(fuCards.failures.length, 0, 'followup_task_started 应清空旧 failures 列表')
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

  // 验证 merge:true 增量扩展策略与 payoffs.values.cells（不传 players，且支持外层 {"merge":true,"spec":...}）与参数深度合并
  const mergeDir = path.join(dayDir, 'case-20261114-merge-001')
  fs.mkdirSync(mergeDir, { recursive: true })
  fs.writeFileSync(path.join(mergeDir, 'header.json'), JSON.stringify({ case_id: 'case-20261114-merge-001', mode: 'scenario', playbook: 'pricing_response' }))
  fs.writeFileSync(path.join(mergeDir, 'case.jsonl'), [
    JSON.stringify({ seq: 1, event: { type: 'phase_entered', from: 'intake', to: 'model_build', reason: 'build' } }),
    JSON.stringify({
      seq: 2,
      event: {
        type: 'tool_call_started',
        call_id: 'm1',
        name: 'gamespec_build',
        arguments: JSON.stringify({
          game_id: 'g-merge',
          players: [{ id: 'us', name: '我方', relation_to_game: 'Player' }, { id: 'comp', name: '竞对', relation_to_game: 'Player' }],
          strategies: { us: ['稳价', '跟降'], comp: ['原价', '降价'] },
          params: { p_a: { name: '基准利润', value: { kind: 'literal', value: 100 }, source: { kind: 'user_confirmed', checkpoint: 'cp2' } } },
          payoffs: {
            form: 'normal',
            values: {
              cells: {
                '稳价,原价': { us: { kind: 'literal', value: 100 }, comp: { kind: 'literal', value: 90 } }
              }
            }
          }
        })
      }
    }),
    JSON.stringify({ seq: 3, event: { type: 'tool_call_finished', call_id: 'm1', outcome: { status: 'ok', output: '{"ok":true}' } } }),
    JSON.stringify({
      seq: 4,
      event: {
        type: 'tool_call_started',
        call_id: 'm2',
        name: 'gamespec_build',
        arguments: JSON.stringify({
          merge: true,
          spec: {
            strategies: { comp: ['原价', '降价', '加赠'] },
            params: { p_b: { name: '加赠成本', value: { kind: 'literal', value: 15 }, source: { kind: 'user_confirmed', checkpoint: 'cp2' } } },
            payoffs: {
              values: {
                cells: {
                  '稳价,加赠': { us: { kind: 'literal', value: 85 }, comp: { kind: 'literal', value: 95 } }
                }
              }
            }
          }
        })
      }
    }),
    JSON.stringify({ seq: 5, event: { type: 'tool_call_finished', call_id: 'm2', outcome: { status: 'ok', output: '{"ok":true}' } } })
  ].join('\n') + '\n')
  const mergedCards = projectCase('case-20261114-merge-001', tmpHome)
  assert.deepEqual(mergedCards.matrix.rows, ['稳价', '跟降'])
  assert.deepEqual(mergedCards.matrix.cols, ['原价', '降价', '加赠'])
  assert.equal(mergedCards.matrix.form, 'normal', '深合并 payoffs 不得覆盖既有的 form')
  assert.equal(mergedCards.matrix.grid[0].cells[0].cells[0].expr, '100', '深合并 cells 不得抹掉既有单元格')
  assert.equal(mergedCards.matrix.grid[0].cells[2].cells[1].expr, '95', '增量新增策略对应的单元格应正确保留')
  assert.equal(mergedCards.matrix.params, 2)

  // 验证 phase_rewound 清除陈旧 recommend_build 结论与成本投影（不残留上一轮假设产物）
  fs.appendFileSync(path.join(m2Dir, 'case.jsonl'), JSON.stringify({ seq: 10, event: { type: 'phase_rewound', from: 'recommend', to: 'intake', due_to: '重改前提' } }) + '\n')
  const rewoundM2 = projectCase('case-20261113-m2-001', tmpHome)
  assert.equal(rewoundM2.conclusion.headline, null, '回退到 intake 应清空陈旧正式结论')
  assert.equal(rewoundM2.costs.scenarios.length, 0, '回退到 intake 应清空陈旧成本测算')
  assert.ok(rewoundM2.meta.phases.includes('回退→intake'))

  // M2 Remediation: followup_task_started 非条件口径（保留成本/证据与已核准 cp1，不清空非条件上下文）
  const fuCostDir = path.join(dayDir, 'case-20261114-fu-cost')
  fs.mkdirSync(fuCostDir, { recursive: true })
  fs.writeFileSync(path.join(fuCostDir, 'header.json'), JSON.stringify({ case_id: 'case-20261114-fu-cost', mode: 'cost', playbook_id: 'pricing_response' }))
  fs.writeFileSync(path.join(fuCostDir, 'case.jsonl'), [
    JSON.stringify({ seq: 1, event: { type: 'phase_entered', from: 'intake', to: 'intel', playbook: 'pricing_response', reason: 'prior' } }),
    JSON.stringify({ seq: 2, event: { type: 'checkpoint_opened', id: 'cp1', phase: 'intel', items: ['竞品定价'] } }),
    JSON.stringify({ seq: 3, event: { type: 'checkpoint_answered', id: 'cp1', answers: ['确认'], user_actor: 'user' } }),
    JSON.stringify({ seq: 4, event: { type: 'phase_entered', from: 'intel', to: 'model_build', playbook: 'pricing_response', reason: 'prior' } }),
    JSON.stringify({
      seq: 5,
      event: {
        type: 'tool_call_started',
        call_id: 'fc_gs',
        name: 'gamespec_build',
        arguments: JSON.stringify({
          spec: {
            game_id: 'g_noncond',
            players: [{ id: 'us', name: '我方' }, { id: 'comp', name: '竞对' }],
            strategies: { us: ['稳价'], comp: ['跟进'] },
            params: {},
            payoffs: { form: 'normal', values: { cells: { '稳价,跟进': { us: { kind: 'literal', value: 10 }, comp: { kind: 'literal', value: 10 } } } } }
          }
        })
      }
    }),
    JSON.stringify({ seq: 6, event: { type: 'tool_call_finished', call_id: 'fc_gs', outcome: { status: 'ok', output: '{"ok":true}' } } }),
    JSON.stringify({ seq: 7, event: { type: 'checkpoint_opened', id: 'cp2', phase: 'model_build', items: ['矩阵确认'] } }),
    JSON.stringify({ seq: 8, event: { type: 'task_scope_selected', scope: 'cost', reason: '先核算门店成本' } }),
    JSON.stringify({ seq: 9, event: { type: 'tool_call_started', call_id: 'fc_bc', name: 'business_cost', arguments: '{}' } }),
    JSON.stringify({ seq: 10, event: { type: 'tool_call_finished', call_id: 'fc_bc', outcome: { status: 'ok', output: JSON.stringify({ scenario_id: 'sc1', scenario: 'A铺', listed_upfront_cash: 50000, items: [] }) } } }),
    JSON.stringify({ seq: 11, event: { type: 'tool_call_started', call_id: 'fc_cr', name: 'constraint_register', arguments: '{}' } }),
    JSON.stringify({ seq: 12, event: { type: 'tool_call_finished', call_id: 'fc_cr', outcome: { status: 'ok', output: JSON.stringify({ id: 'c1', rule: '预算<=8万' }) } } }),
    JSON.stringify({ seq: 13, event: { type: 'tool_call_started', call_id: 'fc_rr', name: 'research_record', arguments: '{}' } }),
    JSON.stringify({ seq: 14, event: { type: 'tool_call_finished', call_id: 'fc_rr', outcome: { status: 'ok', output: JSON.stringify({ topic: '客流', summary: '日均800' }) } } }),
    JSON.stringify({ seq: 15, event: { type: 'tool_call_started', call_id: 'fc_poi', name: 'poi_survey', arguments: '{}' } }),
    JSON.stringify({ seq: 16, event: { type: 'tool_call_finished', call_id: 'fc_poi', outcome: { status: 'ok', output: JSON.stringify({ poi_name: '地铁口' }) } } }),
    JSON.stringify({ seq: 17, event: { type: 'tool_call_started', call_id: 'fc_ws', name: 'web_search', arguments: '{}' } }),
    JSON.stringify({ seq: 18, event: { type: 'tool_call_finished', call_id: 'fc_ws', outcome: { status: 'ok', output: JSON.stringify({ results: [{ url: 'https://example.com/rent', title: '租金参考' }] }) } } }),
    JSON.stringify({ seq: 19, event: { type: 'scoped_task_completed', result: { scope: 'cost', summary: '成本完成' } } }),
    JSON.stringify({ seq: 20, event: { type: 'tool_call_finished', call_id: 'fc_err', outcome: { status: 'failed', error: 'stale err' } } }),
    JSON.stringify({ seq: 21, event: { type: 'followup_task_started', at: '2026-11-14T01:00:00Z' } })
  ].join('\n') + '\n')
  const fuCostCards = projectCase('case-20261114-fu-cost', tmpHome)
  assert.equal(fuCostCards.meta.scope, null)
  assert.equal(fuCostCards.failures.length, 0)
  assert.equal(fuCostCards.costs.scenarios.length, 1, '非条件 followup 不得清除成本表')
  assert.equal(fuCostCards.costs.constraints.length, 1, '非条件 followup 不得清除约束列表')
  assert.equal(fuCostCards.evidence.researchRecords.length, 1, '非条件 followup 不得清除调研记录')
  assert.equal(fuCostCards.evidence.poiSurveys.length, 1, '非条件 followup 不得清除 POI 踏勘')
  assert.equal(fuCostCards.evidence.sources.length, 1, '非条件 followup 不得清除网络证据')
  assert.ok(fuCostCards.matrix, '非条件 followup 不得抹掉既有博弈矩阵')
  assert.equal(fuCostCards.evidence.checkpoints.find((c) => c.id === 'cp1')?.status, 'answered', '非条件 followup 必须保留已核准的 cp1')
  assert.equal(fuCostCards.evidence.checkpoints.find((c) => c.id === 'cp2')?.status, 'pending', '非条件 followup 不得撤销既有关口 cp2')
  assert.equal(fuCostCards.pendingCheckpoint, 'cp2', '非条件 followup 不得撤销既有关口')
  const fuCostProg = inferProgress(readCase(path.join(fuCostDir, 'case.jsonl')).events)
  assert.equal(fuCostProg.phase, 'model_build', '非条件 followup 不得重置非条件阶段')
  assert.equal(fuCostProg.status, 'waiting')

  // M2 Remediation: followup_task_started 条件口径（清模型/求解/关口/phase=intake，保留成本与证据）
  const fuCondDir = path.join(dayDir, 'case-20261114-fu-cond')
  fs.mkdirSync(fuCondDir, { recursive: true })
  fs.writeFileSync(path.join(fuCondDir, 'header.json'), JSON.stringify({ case_id: 'case-20261114-fu-cond', mode: 'scenario', playbook_id: 'pricing_response' }))
  fs.writeFileSync(path.join(fuCondDir, 'case.jsonl'), [
    JSON.stringify({ seq: 1, event: { type: 'task_scope_selected', scope: 'consultation', reason: '完整推演' } }),
    JSON.stringify({ seq: 2, event: { type: 'phase_entered', from: 'intake', to: 'intel', playbook: 'pricing_response', reason: 'ok' } }),
    JSON.stringify({ seq: 3, event: { type: 'checkpoint_opened', id: 'cp1', phase: 'intel', items: ['情报核实'] } }),
    JSON.stringify({ seq: 4, event: { type: 'checkpoint_answered', id: 'cp1', answers: ['同意'], user_actor: 'user' } }),
    JSON.stringify({ seq: 5, event: { type: 'phase_entered', from: 'intel', to: 'model_build', playbook: 'pricing_response', reason: 'ok' } }),
    JSON.stringify({
      seq: 6,
      event: {
        type: 'tool_call_started',
        call_id: 'cd_gs',
        name: 'gamespec_build',
        arguments: JSON.stringify({
          spec: {
            game_id: 'g_cond',
            players: [{ id: 'us', name: '我方' }, { id: 'comp', name: '竞对' }],
            strategies: { us: ['稳价'], comp: ['跟进'] },
            params: {},
            payoffs: { form: 'normal', values: { cells: { '稳价,跟进': { us: { kind: 'literal', value: 12 }, comp: { kind: 'literal', value: 8 } } } } }
          }
        })
      }
    }),
    JSON.stringify({ seq: 7, event: { type: 'tool_call_finished', call_id: 'cd_gs', outcome: { status: 'ok', output: '{"ok":true}' } } }),
    JSON.stringify({ seq: 8, event: { type: 'checkpoint_opened', id: 'cp2', phase: 'model_build', items: ['矩阵核实'] } }),
    JSON.stringify({ seq: 9, event: { type: 'tool_call_started', call_id: 'cd_sl', name: 'solve_equilibria', arguments: '{}' } }),
    JSON.stringify({ seq: 10, event: { type: 'tool_call_finished', call_id: 'cd_sl', outcome: { status: 'ok', output: JSON.stringify({ route: 'exact', equilibria: [{ payoffs: { us: '12', comp: '8' } }] }) } } }),
    JSON.stringify({ seq: 11, event: { type: 'tool_call_started', call_id: 'cd_bc', name: 'business_cost', arguments: '{}' } }),
    JSON.stringify({ seq: 12, event: { type: 'tool_call_finished', call_id: 'cd_bc', outcome: { status: 'ok', output: JSON.stringify({ scenario_id: 'sc_cond', scenario: 'B铺', listed_upfront_cash: 60000, items: [] }) } } }),
    JSON.stringify({ seq: 13, event: { type: 'tool_call_started', call_id: 'cd_cr', name: 'constraint_register', arguments: '{}' } }),
    JSON.stringify({ seq: 14, event: { type: 'tool_call_finished', call_id: 'cd_cr', outcome: { status: 'ok', output: JSON.stringify({ id: 'c_cond', rule: '回本期<=12月' }) } } }),
    JSON.stringify({ seq: 15, event: { type: 'tool_call_started', call_id: 'cd_cc', name: 'constraint_check', arguments: '{}' } }),
    JSON.stringify({ seq: 16, event: { type: 'tool_call_finished', call_id: 'cd_cc', outcome: { status: 'ok', output: JSON.stringify({ ok: false, violations: ['超时'] }) } } }),
    JSON.stringify({ seq: 17, event: { type: 'tool_call_started', call_id: 'cd_rr', name: 'research_record', arguments: '{}' } }),
    JSON.stringify({ seq: 18, event: { type: 'tool_call_finished', call_id: 'cd_rr', outcome: { status: 'ok', output: JSON.stringify({ topic: '客单价', summary: '22元' }) } } }),
    JSON.stringify({ seq: 19, event: { type: 'tool_call_started', call_id: 'cd_poi', name: 'poi_survey', arguments: '{}' } }),
    JSON.stringify({ seq: 20, event: { type: 'tool_call_finished', call_id: 'cd_poi', outcome: { status: 'ok', output: JSON.stringify({ poi_name: '写字楼' }) } } }),
    JSON.stringify({ seq: 21, event: { type: 'tool_call_started', call_id: 'cd_ws', name: 'web_search', arguments: '{}' } }),
    JSON.stringify({ seq: 22, event: { type: 'tool_call_finished', call_id: 'cd_ws', outcome: { status: 'ok', output: JSON.stringify({ results: [{ url: 'https://example.com/cond', title: '报告' }] }) } } }),
    JSON.stringify({ seq: 23, event: { type: 'tool_call_started', call_id: 'cd_ca', name: 'conditional_analysis', arguments: '{}' } }),
    JSON.stringify({ seq: 24, event: { type: 'tool_call_finished', call_id: 'cd_ca', outcome: { status: 'ok', output: JSON.stringify({ scope: 'conditional', summary: '有条件结论', conditional: { answer_by_assumption: [], missing: [], limitations: ['仅样本期'] } }) } } }),
    JSON.stringify({ seq: 25, event: { type: 'scoped_task_completed', result: { scope: 'conditional', summary: '有条件交付' } } }),
    JSON.stringify({ seq: 26, event: { type: 'followup_task_started', at: '2026-11-14T02:00:00Z' } })
  ].join('\n') + '\n')
  const fuCondCards = projectCase('case-20261114-fu-cond', tmpHome)
  assert.equal(fuCondCards.matrix, null, '条件交付后 followup 应清空旧矩阵')
  assert.equal(fuCondCards.solve, null, '条件交付后 followup 应清空旧求解')
  assert.equal(fuCondCards.scopeDetail.conditional, null, '条件交付后 followup 应清空 conditional')
  assert.equal(fuCondCards.costs.constraintCheck, null, '条件交付后 followup 应清空 constraintCheck')
  assert.equal(fuCondCards.pendingCheckpoint, null, '条件交付后 followup 应撤销未决 cp2')
  assert.equal(fuCondCards.evidence.checkpoints.find((c) => c.id === 'cp1')?.status, 'rewound', '条件交付后 followup 应将已核准 cp1 标为 rewound，不延续旧决策批准')
  assert.equal(fuCondCards.evidence.checkpoints.find((c) => c.id === 'cp2')?.status, 'rewound')
  assert.equal(fuCondCards.costs.scenarios.length, 1, '条件 followup 仍须保留成本表')
  assert.equal(fuCondCards.costs.constraints.length, 1, '条件 followup 仍须保留约束列表')
  assert.equal(fuCondCards.evidence.researchRecords.length, 1, '条件 followup 仍须保留调研记录')
  assert.equal(fuCondCards.evidence.poiSurveys.length, 1, '条件 followup 仍须保留 POI 踏勘')
  assert.equal(fuCondCards.evidence.sources.length, 1, '条件 followup 仍须保留网络证据')
  const fuCondProg = inferProgress(readCase(path.join(fuCondDir, 'case.jsonl')).events)
  assert.equal(fuCondProg.phase, 'intake')
  assert.equal(fuCondProg.status, null)

  // 仅通过 conditional_analysis 工具（无 scoped_task_completed）后的 followup 也须将已核准 cp1 标为 rewound
  const fuCondToolDir = path.join(dayDir, 'case-20261114-fu-cond-tool')
  fs.mkdirSync(fuCondToolDir, { recursive: true })
  fs.writeFileSync(path.join(fuCondToolDir, 'header.json'), JSON.stringify({ case_id: 'case-20261114-fu-cond-tool', mode: 'scenario', playbook_id: 'pricing_response' }))
  fs.writeFileSync(path.join(fuCondToolDir, 'case.jsonl'), [
    JSON.stringify({ seq: 1, event: { type: 'task_scope_selected', scope: 'consultation', reason: '完整推演' } }),
    JSON.stringify({ seq: 2, event: { type: 'phase_entered', from: 'intake', to: 'intel', playbook: 'pricing_response', reason: 'ok' } }),
    JSON.stringify({ seq: 3, event: { type: 'checkpoint_opened', id: 'cp1', phase: 'intel', items: ['情报核实'] } }),
    JSON.stringify({ seq: 4, event: { type: 'checkpoint_answered', id: 'cp1', answers: ['同意'], user_actor: 'user' } }),
    JSON.stringify({ seq: 5, event: { type: 'tool_call_started', call_id: 'ct_ca', name: 'conditional_analysis', arguments: '{}' } }),
    JSON.stringify({ seq: 6, event: { type: 'tool_call_finished', call_id: 'ct_ca', outcome: { status: 'ok', output: JSON.stringify({ scope: 'conditional', summary: '有条件结论', conditional: { limitations: ['缺毛利'] } }) } } }),
    JSON.stringify({ seq: 7, event: { type: 'followup_task_started', at: '2026-11-14T02:10:00Z' } })
  ].join('\n') + '\n')
  const fuCondToolCards = projectCase('case-20261114-fu-cond-tool', tmpHome)
  assert.equal(fuCondToolCards.evidence.checkpoints.find((c) => c.id === 'cp1')?.status, 'rewound', 'conditional_analysis + followup_task_started 必须将已核准 cp1 标为 rewound')

  // M2 Remediation: 同阶段原地重审 (from == to) 与跨阶段作废回退（含 answered 下游关口标记 rewound）
  const rwInplaceDir = path.join(dayDir, 'case-20261114-rw-inplace')
  fs.mkdirSync(rwInplaceDir, { recursive: true })
  fs.writeFileSync(path.join(rwInplaceDir, 'header.json'), JSON.stringify({ case_id: 'case-20261114-rw-inplace', mode: 'scenario', playbook_id: 'pricing_response' }))
  fs.writeFileSync(path.join(rwInplaceDir, 'case.jsonl'), [
    JSON.stringify({ seq: 1, event: { type: 'phase_entered', from: 'intake', to: 'intel', playbook: 'pricing_response', reason: 'ok' } }),
    JSON.stringify({ seq: 2, event: { type: 'checkpoint_opened', id: 'cp1', phase: 'intel', items: ['情报'] } }),
    JSON.stringify({ seq: 3, event: { type: 'checkpoint_answered', id: 'cp1', answers: ['确认'], user_actor: 'user' } }),
    JSON.stringify({ seq: 4, event: { type: 'phase_entered', from: 'intel', to: 'model_build', playbook: 'pricing_response', reason: 'ok' } }),
    JSON.stringify({
      seq: 5,
      event: {
        type: 'tool_call_started',
        call_id: 'rw_gs1',
        name: 'gamespec_build',
        arguments: JSON.stringify({
          spec: {
            game_id: 'g_rw1',
            players: [{ id: 'us', name: '我方' }, { id: 'comp', name: '竞对' }],
            strategies: { us: ['稳价'], comp: ['跟进'] },
            params: {},
            payoffs: { form: 'normal', values: { cells: { '稳价,跟进': { us: { kind: 'literal', value: 20 }, comp: { kind: 'literal', value: 20 } } } } }
          }
        })
      }
    }),
    JSON.stringify({ seq: 6, event: { type: 'tool_call_finished', call_id: 'rw_gs1', outcome: { status: 'ok', output: '{"ok":true}' } } }),
    JSON.stringify({ seq: 7, event: { type: 'checkpoint_opened', id: 'cp2', phase: 'model_build', items: ['建模'] } }),
    JSON.stringify({ seq: 8, event: { type: 'phase_rewound', from: 'model_build', to: 'model_build', due_to: '重审当前关口' } })
  ].join('\n') + '\n')
  const rwInplaceCards = projectCase('case-20261114-rw-inplace', tmpHome)
  assert.equal(rwInplaceCards.matrix, null, 'from === to 原地重审应清空当前阶段 model_build 的矩阵')
  assert.equal(rwInplaceCards.pendingCheckpoint, null, 'from === to 应撤销当前阶段的未决 cp2')
  assert.equal(rwInplaceCards.evidence.checkpoints[0].status, 'answered', '上游 cp1 应保持 answered')
  assert.equal(rwInplaceCards.evidence.checkpoints[1].status, 'rewound', '当前阶段 cp2 应标为 rewound')

  // 继续推进并触发作废回退（校验 answered 下游关口也会标为 rewound）
  fs.appendFileSync(path.join(rwInplaceDir, 'case.jsonl'), [
    JSON.stringify({
      seq: 9,
      event: {
        type: 'tool_call_started',
        call_id: 'rw_gs2',
        name: 'gamespec_build',
        arguments: JSON.stringify({
          spec: {
            game_id: 'g_rw2',
            players: [{ id: 'us', name: '我方' }, { id: 'comp', name: '竞对' }],
            strategies: { us: ['稳价'], comp: ['跟进'] },
            params: {},
            payoffs: { form: 'normal', values: { cells: { '稳价,跟进': { us: { kind: 'literal', value: 30 }, comp: { kind: 'literal', value: 15 } } } } }
          }
        })
      }
    }),
    JSON.stringify({ seq: 10, event: { type: 'tool_call_finished', call_id: 'rw_gs2', outcome: { status: 'ok', output: '{"ok":true}' } } }),
    JSON.stringify({ seq: 11, event: { type: 'checkpoint_opened', id: 'cp2', phase: 'model_build', items: ['重建建模'] } }),
    JSON.stringify({ seq: 12, event: { type: 'checkpoint_answered', id: 'cp2', answers: ['确认'], user_actor: 'user' } }),
    JSON.stringify({ seq: 13, event: { type: 'phase_entered', from: 'model_build', to: 'solve', playbook: 'pricing_response', reason: 'ok' } }),
    JSON.stringify({ seq: 14, event: { type: 'tool_call_started', call_id: 'rw_sl2', name: 'solve_equilibria', arguments: '{}' } }),
    JSON.stringify({ seq: 15, event: { type: 'tool_call_finished', call_id: 'rw_sl2', outcome: { status: 'ok', output: JSON.stringify({ route: 'exact', equilibria: [{ payoffs: { us: '30', comp: '15' } }] }) } } }),
    JSON.stringify({ seq: 16, event: { type: 'checkpoint_opened', id: 'cp3', phase: 'solve', items: ['均衡核实'] } }),
    JSON.stringify({ seq: 17, event: { type: 'checkpoint_answered', id: 'cp3', answers: ['确认'], user_actor: 'user' } }),
    JSON.stringify({ seq: 18, event: { type: 'phase_entered', from: 'solve', to: 'select', playbook: 'pricing_response', reason: 'ok' } }),
    JSON.stringify({ seq: 19, event: { type: 'phase_rewound', from: 'select', to: 'model_build', due_to: '用户更新约束或上下文，受影响的下游产物与关口确认已作废' } })
  ].join('\n') + '\n')
  const rwInvalCards = projectCase('case-20261114-rw-inplace', tmpHome)
  assert.equal(rwInvalCards.matrix, null, '作废回退到 model_build 应清空陈旧矩阵')
  assert.equal(rwInvalCards.solve, null, '作废回退到 model_build 应清空下游求解')
  assert.equal(rwInvalCards.evidence.checkpoints[0].status, 'answered', '上游 cp1 应维持 answered')
  assert.equal(rwInvalCards.evidence.checkpoints[2].status, 'rewound', '已答下游 cp2 应标为 rewound')
  assert.equal(rwInvalCards.evidence.checkpoints[3].status, 'rewound', '已答下游 cp3 应标为 rewound')

  // M2 Remediation: request_rewind(to=model_build) + gamespec_build(merge=true) 保留既有模型与 cp2
  const reqRwDir = path.join(dayDir, 'case-20261114-rw-request')
  fs.mkdirSync(reqRwDir, { recursive: true })
  fs.writeFileSync(path.join(reqRwDir, 'header.json'), JSON.stringify({ case_id: 'case-20261114-rw-request', mode: 'scenario', playbook_id: 'pricing_response' }))
  fs.writeFileSync(path.join(reqRwDir, 'case.jsonl'), [
    JSON.stringify({ seq: 1, event: { type: 'phase_entered', from: 'intake', to: 'model_build', playbook: 'pricing_response', reason: 'ok' } }),
    JSON.stringify({
      seq: 2,
      event: {
        type: 'tool_call_started',
        call_id: 'rq_gs1',
        name: 'gamespec_build',
        arguments: JSON.stringify({
          spec: {
            game_id: 'g_req',
            players: [{ id: 'us', name: '我方' }, { id: 'comp', name: '竞对' }],
            strategies: { us: ['稳价', '跟降'], comp: ['原价', '降价'] },
            params: { p_a: { name: '基准成本', value: { kind: 'literal', value: 10 }, source: { kind: 'user_confirmed', checkpoint: 'cp2' } } },
            payoffs: { form: 'normal', values: { cells: { '稳价,原价': { us: { kind: 'literal', value: 100 }, comp: { kind: 'literal', value: 90 } } } } }
          }
        })
      }
    }),
    JSON.stringify({ seq: 3, event: { type: 'tool_call_finished', call_id: 'rq_gs1', outcome: { status: 'ok', output: '{"ok":true}' } } }),
    JSON.stringify({ seq: 4, event: { type: 'checkpoint_opened', id: 'cp2', phase: 'model_build', items: ['建模'] } }),
    JSON.stringify({ seq: 5, event: { type: 'checkpoint_answered', id: 'cp2', answers: ['确认'], user_actor: 'user' } }),
    JSON.stringify({ seq: 6, event: { type: 'phase_entered', from: 'model_build', to: 'solve', playbook: 'pricing_response', reason: 'ok' } }),
    JSON.stringify({ seq: 7, event: { type: 'tool_call_started', call_id: 'rq_sl', name: 'solve_equilibria', arguments: '{}' } }),
    JSON.stringify({ seq: 8, event: { type: 'tool_call_finished', call_id: 'rq_sl', outcome: { status: 'ok', output: JSON.stringify({ route: 'exact', equilibria: [{ payoffs: { us: '100', comp: '90' } }] }) } } }),
    JSON.stringify({ seq: 9, event: { type: 'checkpoint_opened', id: 'cp3', phase: 'solve', items: ['均衡'] } }),
    JSON.stringify({ seq: 10, event: { type: 'phase_entered', from: 'solve', to: 'select', playbook: 'pricing_response', reason: 'ok' } }),
    JSON.stringify({ seq: 11, event: { type: 'phase_rewound', from: 'select', to: 'model_build', due_to: '需要补充新策略（用户确认：同意回退到 model_build 修改参数）' } }),
    JSON.stringify({
      seq: 12,
      event: {
        type: 'tool_call_started',
        call_id: 'rq_gs2',
        name: 'gamespec_build',
        arguments: JSON.stringify({
          merge: true,
          spec: {
            strategies: { comp: ['原价', '降价', '加赠'] },
            params: { p_b: { name: '加赠成本', value: { kind: 'literal', value: 15 }, source: { kind: 'user_confirmed', checkpoint: 'cp2' } } }
          }
        })
      }
    }),
    JSON.stringify({ seq: 13, event: { type: 'tool_call_finished', call_id: 'rq_gs2', outcome: { status: 'ok', output: '{"ok":true}' } } })
  ].join('\n') + '\n')
  const reqRwCards = projectCase('case-20261114-rw-request', tmpHome)
  assert.equal(reqRwCards.solve, null, 'request_rewind 回到 model_build 应清除下游求解')
  assert.equal(reqRwCards.pendingCheckpoint, null, '下游未决 cp3 应撤销')
  assert.equal(reqRwCards.evidence.checkpoints.find((c) => c.id === 'cp2')?.status, 'answered', 'request_rewind(model_build) 应保留 cp2 为 answered')
  assert.equal(reqRwCards.evidence.checkpoints.find((c) => c.id === 'cp3')?.status, 'rewound', 'request_rewind(model_build) 应将下游 cp3 标为 rewound')
  assert.deepEqual(reqRwCards.matrix.rows, ['稳价', '跟降'], 'request_rewind(model_build) + merge:true 应保留既有行策略')
  assert.deepEqual(reqRwCards.matrix.cols, ['原价', '降价', '加赠'])
  assert.equal(reqRwCards.matrix.params, 2)

  // M2 Remediation: deepMergeJson 含判别字段 kind/form 切换整对象替换，不得残留旧变体字段
  const varDir = path.join(dayDir, 'case-20261114-variant-merge')
  fs.mkdirSync(varDir, { recursive: true })
  fs.writeFileSync(path.join(varDir, 'header.json'), JSON.stringify({ case_id: 'case-20261114-variant-merge', mode: 'scenario', playbook_id: 'pricing_response' }))
  fs.writeFileSync(path.join(varDir, 'case.jsonl'), [
    JSON.stringify({
      seq: 1,
      event: {
        type: 'tool_call_started',
        call_id: 'vm1',
        name: 'gamespec_build',
        arguments: JSON.stringify({
          spec: {
            game_id: 'g_var',
            players: [{ id: 'us', name: '我方' }, { id: 'comp', name: '竞对' }],
            strategies: { us: ['A'], comp: ['B'] },
            params: { p1: { name: '弹性', value: { kind: 'literal', value: 1.2 }, source: { kind: 'assumed', note: '暂估' } } },
            payoffs: { form: 'normal', values: { cells: { 'A,B': { us: { kind: 'literal', value: 10 }, comp: { kind: 'literal', value: 20 } } } } }
          }
        })
      }
    }),
    JSON.stringify({ seq: 2, event: { type: 'tool_call_finished', call_id: 'vm1', outcome: { status: 'ok', output: '{"ok":true}' } } }),
    JSON.stringify({
      seq: 3,
      event: {
        type: 'tool_call_started',
        call_id: 'vm2',
        name: 'gamespec_build',
        arguments: JSON.stringify({
          merge: true,
          spec: {
            params: { p1: { source: { kind: 'user_confirmed', checkpoint: 'cp2' } } },
            payoffs: { form: 'extensive', root: { kind: 'decision', player: 'us' } }
          }
        })
      }
    }),
    JSON.stringify({ seq: 4, event: { type: 'tool_call_finished', call_id: 'vm2', outcome: { status: 'ok', output: '{"ok":true}' } } })
  ].join('\n') + '\n')
  const varCards = projectCase('case-20261114-variant-merge', tmpHome)
  assert.equal(varCards.assumptions.params[0].sourceKind, 'user_confirmed')
  assert.equal(varCards.assumptions.params[0].checkpoint, 'cp2')
  assert.equal(varCards.assumptions.gaps.length, 0)
  assert.equal(varCards.matrix.form, 'extensive')
  assert.equal(varCards.matrix.hasPayoffCells, false, 'form 切换为 extensive 时不得残留旧 normal.values.cells')

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
    assert.ok(badHealth.problems.some((p) => p.code === 'mcp_tools_missing'))

    const unknownHealth = getHealthStatus({ binPath: process.execPath, home: tmpHome })
    assert.equal(unknownHealth.ok, false)
    assert.equal(unknownHealth.mcpTools.status, 'unknown')
    assert.ok(unknownHealth.problems.some((p) => p.code === 'mcp_tools_unknown'))

    const readyHealth = getHealthStatus({ binPath: process.execPath, mcpToolCount: 39, home: tmpHome })
    assert.equal(readyHealth.ok, true)
    assert.equal(readyHealth.mcpTools.status, 'ok')
    assert.equal(readyHealth.problems.length, 0)
  } finally {
    if (prevShHome === undefined) delete process.env.SHANGYI_HOME
    else process.env.SHANGYI_HOME = prevShHome
  }

  console.log('✓ dsh-plugin-shangyi 宿主投影、waiting/stuck 区分、phase_rewound/followup 状态追踪、受控关口结算、可信账本、降级抽屉主题令牌与 KB 越界防护全部通过')
} finally {
  fs.rmSync(tmpHome, { recursive: true, force: true })
}
