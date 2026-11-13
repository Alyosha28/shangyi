import test from 'node:test';
import assert from 'node:assert/strict';
import {caseExportMarkdown} from '../js/views/case.js';

// UI-015：「保存记录」原来只有原始 JSON（86 KB，header/artifacts/events 套着结构），
// 用户想之后阅读、打印或分享时根本用不了。现在先出一份能直接读的 Markdown。
// 这里锁的是**那份文件里必须有什么**：问题原文、每次结果、缺口、建议、
// 适用条件、来源；以及**不许出现什么**：不得把没做的事写成做了。

const record = (extra = {}) => ({
  question: '帮我算算开店每月要花多少钱',
  header: { mode: 'scenario', playbook_id: 'market_entry', created_at: '2026-10-04T02:30:00Z' },
  stats: { phases: ['archived'], n_events: 12 },
  artifacts: {
    task_results: [{
      scope: 'cost', status: 'partial', at: '2026-10-04T02:31:00Z',
      summary: '每月固定支出约 16,500 元；一次性装修 50,000 元。',
      missing: ['装修款的付款节奏'],
      source_urls: ['https://example.com/listing'],
    }],
    latest_task: undefined,
    recommendation: {
      headline: '先谈免租期',
      actions: [{ action: '接受小幅涨租', rationale: '换更长免租期', confidence: 'medium' }],
      do_not: ['直接答应涨 20%'],
      caveats: ['只在租金谈判这一步成立'],
    },
    gamespec: {
      spec: {
        players: [
          { id: 'me', name: '我', relation_to_game: 'player' },
          { id: 'rival', name: '另一家店', relation_to_game: 'player' },
        ],
        strategies: { me: ['保持原价', '跟着降价'], rival: ['保持原价', '降价'] },
        payoffs: { form: 'normal', values: { cells: {
          '保持原价,保持原价': { me: { value: 1 }, rival: { value: 1 } },
          '保持原价,降价': { me: { value: -1 }, rival: { value: 2 } },
          '跟着降价,保持原价': { me: { value: 2 }, rival: { value: -1 } },
          '跟着降价,降价': { me: { value: 0 }, rival: { value: 0 } },
        } } },
        params: { demand: { name: '区域月客流', value: { value: 400000 }, unit: '人' } },
      },
    },
    web_sources: [{ title: '租赁挂牌页', url: 'https://example.com/listing' }],
    report: { ledger: { tools: ['solve_equilibria'] } },
  },
  ...extra,
});

test('可读文件里有问题原文、结果、缺口、建议与适用范围', () => {
  const md = caseExportMarkdown(record());
  assert.match(md, /^# 帮我算算开店每月要花多少钱/);
  assert.match(md, /## 我当时的问法/);
  assert.match(md, /## 结果/);
  assert.match(md, /每月固定支出约 16,500 元/);
  assert.match(md, /押金|装修款的付款节奏/, '缺口必须进可读文件');
  assert.match(md, /## 建议/);
  assert.match(md, /先谈免租期/);
  assert.match(md, /接受小幅涨租/);
  assert.match(md, /### 适用条件与限制/);
  assert.match(md, /只在租金谈判这一步成立/);
  assert.match(md, /### 需要避开的选择/);
  assert.match(md, /## 来源与怎么算的/);
  assert.match(md, /https:\/\/example\.com\/listing/, '来源链接必须进可读文件');
});

test('收益矩阵转成 Markdown 表，行列按 players 数组顺序', () => {
  const md = caseExportMarkdown(record());
  assert.match(md, /## 好坏对照/);
  assert.match(md, /\| 我 \\ 另一家店 \| \*\*保持原价\*\* \| \*\*降价\*\* \|/);
  assert.match(md, /\| \*\*保持原价\*\* \| 1 \/ 1 \| -1 \/ 2 \|/);
  assert.match(md, /\| \*\*跟着降价\*\* \| 2 \/ -1 \| 0 \/ 0 \|/);
});

test('多次要求的整理逐次列出，并说明哪次是部分交付', () => {
  const first = {
    scope: 'research', status: 'complete', summary: '主办方是市文旅局。', missing: [], at: '2026-10-04T02:00:00Z',
  };
  const second = {
    scope: 'cost', status: 'partial', summary: '每月固定支出约 16,500 元。', missing: ['押金金额未取得'], at: '2026-10-04T02:31:00Z',
  };
  const md = caseExportMarkdown(record({
    artifacts: { task_results: [first, second], latest_task: second },
  }));
  assert.match(md, /第 1 次/);
  assert.match(md, /第 2 次/);
  assert.match(md, /资料整理/);
  assert.match(md, /费用整理/);
  assert.match(md, /仍缺少的信息/);
  assert.match(md, /押金金额未取得/);
});

test('没有内容时不导出空白文件（前端据此禁用并提示）', () => {
  const md = caseExportMarkdown(record({
    question: '',
    artifacts: { task_results: [], recommendation: null, gamespec: { spec: {} } },
  }));
  // 至少要有标题与说明，不能是空串
  assert.ok(md.trim().length > 0);
  assert.ok(!md.includes('## 结果'), '没有结果就不该有结果一节');
});

test('原始 JSON 仍在（页面第二个按钮），可读文件只做转述不改数字', () => {
  const data = record();
  const md = caseExportMarkdown(data);
  // 可读文件里的数字必须与记录里的一致，不允许四舍五入或美化
  assert.ok(md.includes('16,500'));
  assert.ok(md.includes('50,000'));
  const raw = JSON.stringify(data);
  assert.ok(raw.includes('16500') || raw.includes('16,500'));
});

// ── UI-013：费用整理要把「你列的项目算清没」和「清单是否声明完整」分开说 ──
import {costScopeNote} from '../js/util.js';

test('列的项目都算清时明说算清了，并给出已算出的数', () => {
  const note = costScopeNote({
    scope: 'cost', status: 'partial',
    cost_scenarios: [{
      scenario: '南京东路铺位', listed_monthly_fixed_cost: 16500, listed_upfront_cash: 50000,
      amounts_complete: true, cash_classification_complete: true, costs_complete: false,
    }],
  });
  assert.match(note, /你列的项目都算清了/);
  assert.match(note, /每月固定 16500/);
  assert.match(note, /一次性现金 50000/);
  // 清单未声明完整 ≠ 你要的没算完：这句话必须说的是范围，不是「没算完」
  assert.match(note, /没列进来的费用不在这次范围内/);
  assert.ok(!/没算清/.test(note), '列的项目都算清了就不该再说没算清');
});

test('列的项目里还有没算清的，点名是哪几项', () => {
  const note = costScopeNote({
    scope: 'cost',
    cost_scenarios: [{
      amounts_complete: false, cash_classification_complete: false, costs_complete: true,
      missing_amount_items: [{ name: '装修款' }],
      unclassified_items: [{ name: '押金' }],
    }],
  });
  assert.match(note, /你列的项目里还有 装修款、押金没算清/);
  assert.match(note, /金额没取得或性质待定/);
  assert.ok(!note.includes('没列进来'), '清单声明完整时不该提范围外的缺口');
});

test('没有 cost_scenarios 就不说话', () => {
  assert.equal(costScopeNote({ scope: 'cost', status: 'partial' }), '');
  assert.equal(costScopeNote(undefined), '');
  assert.equal(costScopeNote({ scope: 'research' }), '');
});
