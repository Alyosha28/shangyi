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
  readKbDoc
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

  console.log('✓ dsh-plugin-shangyi 宿主投影、受控关口结算、可信账本与 KB 越界防护全部通过')
} finally {
  fs.rmSync(tmpHome, { recursive: true, force: true })
}
