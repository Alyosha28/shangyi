import test from 'node:test';
import assert from 'node:assert/strict';
import {mergeConsultations,filterConsultations} from '../js/views/cases.js';
import {TASK_LABEL,TASK_FILTERS} from '../js/util.js';

// UI-016：案例目录按 **UTC** 分片（run.rs 里写明了这是刻意的），于是中国用户
// 凌晨或深夜跑的咨询会落在"前一天"的目录里；列表却把那个目录名直接当展示日期，
// 于是"今天刚问过的问题"显示成昨天，按今天日期也搜不到。
//
// 口径统一为：**用创建时刻换算用户本地日期**；案例路径只当标识（datePath 保留），
// 不再参与展示。搜索两种日期都认——用户可能照着目录日期搜。

const pad = (n) => String(n).padStart(2, '0');
/** 与实现同口径的本地日期（测试里独立算一遍期望值）。 */
function localDateOf(instant) {
  return [instant.getFullYear(), pad(instant.getMonth() + 1), pad(instant.getDate())].join('-');
}

/** 造一个「UTC 日期与本地日期必然不同」的时刻：东时区取傍晚，西时区取凌晨。 */
function shiftedInstant() {
  const eastOfUtc = new Date('2026-06-01T00:00:00Z').getTimezoneOffset() < 0;
  return {
    instant: eastOfUtc ? new Date('2026-10-03T20:00:00Z') : new Date('2026-10-03T02:00:00Z'),
    eastOfUtc,
  };
}

test('案例展示本地日期，不把 UTC 分片目录名当日期', () => {
  const {instant, eastOfUtc} = shiftedInstant();
  const utcShard = instant.toISOString().slice(0, 10);
  assert.equal(utcShard, '2026-10-03', '夹具本身要是 UTC 的 10-03');
  const cases = [{
    case_id: 'case-20261003-cost-200000-4242',
    date: utcShard,
    mode: 'cost',
    playbook_id: 'store_location_research',
    created_at: instant.toISOString(),
    question: '整理开店每月的固定支出',
    mtime: 1,
  }];
  const rows = mergeConsultations(cases, [], []);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].date, localDateOf(instant),
    `应按创建时刻换算本地日期（本机${eastOfUtc ? '东' : '西'}时区）`);
  assert.equal(rows[0].datePath, utcShard, 'UTC 分片名作为标识保留，不该被改掉');
  assert.notEqual(rows[0].date, rows[0].datePath, '这个夹具必须真的跨天，否则测不出偏移');
});

test('按本地日期搜得到，照 UTC 目录日期也搜得到', () => {
  const {instant} = shiftedInstant();
  const utcShard = instant.toISOString().slice(0, 10);
  const rows = mergeConsultations([{
    case_id: 'case-20261003-cost-200000-4242', date: utcShard, mode: 'cost',
    playbook_id: 'p', created_at: instant.toISOString(), question: '整理开店每月的固定支出', mtime: 1,
  }], [], []);
  assert.equal(filterConsultations(rows, 'all', localDateOf(instant)).length, 1,
    '按用户本地日期（"今天问的"）必须能搜到');
  assert.equal(filterConsultations(rows, 'all', utcShard).length, 1,
    '照着案例目录日期搜也不该落空——两种口径都是事实');
});

test('有案例与无案例的对话用同一口径，凌晨创建的也显示当天', () => {
  const {instant} = shiftedInstant();
  const local = localDateOf(instant);
  const withCase = mergeConsultations([], [{
    id: 'c1', initial: '整理开店每月的固定支出', status: 'completed', created_at: instant.getTime() / 1000,
    case: '2026-10-03/case-20261003-cost-200000-4242', mode: 'cost',
  }], []);
  const withoutCase = mergeConsultations([], [{
    id: 'c2', initial: '续租怎么谈', status: 'completed', created_at: instant.getTime() / 1000, mode: 'negotiation',
  }], []);
  assert.equal(withCase[0].date, local);
  assert.equal(withoutCase[0].date, local, '有无关联案例必须同一口径');
});

test('没有 created_at 时从 case_id 的 UTC 时间推，推不出才退回目录名', () => {
  const {instant} = shiftedInstant();
  const rows = mergeConsultations([], [{
    id: 'c3', status: 'completed', mode: 'cost',
    case: '2026-10-03/case-20261003-cost-200000-4242',
  }], []);
  assert.equal(rows[0].date, localDateOf(instant), 'case_id 里带 UTC 时分秒，能推出创建时刻');
  const opaque = mergeConsultations([], [{ id: 'c4', status: 'completed', mode: 'cost', case: 'weird/path' }], []);
  assert.equal(opaque[0].date, 'weird', '什么都推不出时退回原值，不编日期');
});

test('兜底标题里的时间也是本地时刻，不与列表日期差一天', () => {
  const {instant} = shiftedInstant();
  // 抽不到问题原文时才会走 fallbackTitle：question 为空
  const rows = mergeConsultations([{
    case_id: 'case-20261003-cost-200000-4242', date: '2026-10-03', mode: 'cost',
    playbook_id: 'p', created_at: instant.toISOString(), question: '', mtime: 1,
  }], [], []);
  const stamp = `${pad(instant.getMonth() + 1)}-${pad(instant.getDate())} ${pad(instant.getHours())}:${pad(instant.getMinutes())}`;
  assert.ok(rows[0].title.includes(stamp), `标题应含本地时间 ${stamp}，实际：${rows[0].title}`);
  assert.ok(!rows[0].title.includes('20:00') || instant.getHours() === 20,
    '不能再直接显示 UTC 字符串里的时刻');
});

// ── UI-014：列表按「这次做了什么」归类，不按内部分析模式 ──────────────────
// 原形态：一次费用整理的记录在列表里显示「情景分析」，筛选项也是
// scenario/whatif/discover——用户得先看懂内部分析模式，才认出自己刚提交的是什么。
test('费用整理/资料整理的记录按任务归类，不按分析模式', () => {
  const rows = mergeConsultations([], [
    { id: 'c-cost', initial: '整理开店每月的固定支出', status: 'partial', created_at: 100,
      task_scope: 'cost', mode: 'scenario' },
    { id: 'c-research', initial: '查一下咖啡文化节的主办方', status: 'completed', created_at: 200,
      task_scope: 'research', mode: 'scenario' },
    { id: 'c-consult', initial: '隔壁降价我要不要跟', status: 'completed', created_at: 300,
      mode: 'whatif' },
  ], []);
  // 按 mtime 倒序排，所以这里比集合不比顺序
  assert.deepEqual(rows.map((r) => r.kind).sort(), ['consultation', 'cost', 'research']);
  assert.equal(filterConsultations(rows, 'cost', '').length, 1);
  assert.equal(filterConsultations(rows, 'research', '').length, 1);
  assert.equal(filterConsultations(rows, 'consultation', '').length, 1);
  assert.equal(filterConsultations(rows, 'consultation', '')[0].id, 'c-consult',
    '没标 task_scope 的完整咨询归到经营分析，而不是散落');
});

test('不认识的 task_scope 退回经营分析，不显示成空白', () => {
  const rows = mergeConsultations([], [
    { id: 'c-odd', initial: '怪值', status: 'completed', created_at: 100, task_scope: 'future_scope' },
  ], []);
  assert.equal(rows[0].kind, 'consultation');
  assert.equal(TASK_LABEL[rows[0].kind], '经营分析', '标签必须落在一个看得懂的词上');
});

test('筛选词与列表标记用同一套词', () => {
  assert.deepEqual(TASK_FILTERS.map(([id]) => id), ['all', 'consultation', 'research', 'cost', 'research_and_cost', 'model']);
  for (const [id, label] of TASK_FILTERS) {
    if (id === 'all') continue;   // 「全部」没有对应标记，跳过
    assert.equal(TASK_LABEL[id], label, `筛选项 ${id} 的用词必须与列表标记一致`);
  }
});
