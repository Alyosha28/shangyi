import test from 'node:test';
import assert from 'node:assert/strict';
import {numbersBundle, numbersReport} from '../js/views/playground.js';

/** 最小 DOM 桩：面板只读 input 的 value。 */
function view(values) {
  return {querySelector: (selector) => {
    const id = selector.replace('#pg-n-', '');
    return id in values ? {value: String(values[id])} : null;
  }};
}
const FIELDS = {traffic: 400000, share: 6, price: 24, margin: 60, fixed: 330000, upfront: 1200000,
  rshare: 35, rprice: 22, rfixed: 900000, cut: 10, take: 10, rtake: 20};

test('面板数字被换算成两家动作与乘数', () => {
  const bundle = numbersBundle(view(FIELDS), {me: '我（喜茶）', rival: '周边竞品'});
  assert.equal(bundle.market.traffic, 400000);
  assert.equal(bundle.me.share, 0.06);
  assert.equal(bundle.me.moves[0].name, '原价开店');
  assert.equal(bundle.me.moves[1].name, '降价10%');
  assert.equal(bundle.me.moves[1].price_mult, 0.9);
  assert.equal(bundle.me.moves[2].closed, true);
  // 抢走对方 10% 客流 ⇒ 对方份额 ×0.9；我多拿 35%×10% 绝对客流 ⇒ 份额 ×(1+0.035/0.06)
  assert.equal(bundle.me.moves[1].opponent_share_mult, 0.9);
  assert.ok(Math.abs(bundle.me.moves[1].share_mult - (1 + 0.035 / 0.06)) < 1e-9);
  assert.equal(bundle.rival.moves[1].opponent_share_mult, 0.8);
  assert.ok(Math.abs(bundle.rival.moves[1].share_mult - (1 + 0.012 / 0.35)) < 1e-9);
  assert.equal(bundle.rival.name, '周边竞品');
});

test('不填毛利率乘数：交给编译器按单位成本推导', () => {
  const bundle = numbersBundle(view(FIELDS), {me: '我', rival: '对方'});
  for (const move of [...bundle.me.moves, ...bundle.rival.moves]) {
    assert.ok(!('margin_mult' in move), '界面不猜毛利率，交给算式');
  }
});

test('编译失败时只列缺口，不给假矩阵', () => {
  const html = numbersReport({ok: false, incomplete: ['我方的「margin」缺失或不合法']}, {me: '我', rival: '对方'});
  assert.match(html, /还差些数字/);
  assert.match(html, /我方的「margin」缺失或不合法/);
  assert.ok(!html.includes('<table'));
  assert.ok(!html.includes('元/月'));
});

test('编译成功时矩阵、回本、未建模与算式都摆出来', () => {
  const html = numbersReport({
    ok: true,
    cells: [{me_move: '原价开店', rival_move: '照常', me_profit: 15600, rival_profit: 948000,
             formula: {me: '客流 40.00万 × 份额 6% = 1.56万', rival: '对方算式'}}],
    payback_months: {'原价开店,照常': 76.9},
    unmodeled: ['「原价开店」没声明对对方份额的影响，按 1.0 算'],
    warning: [],
  }, {me: '我', rival: '对方'});
  assert.match(html, /按数字算出来的矩阵/);
  assert.match(html, /1.6 万/);            // 15600 元压成万元
  assert.match(html, /94\.8 万/);          // 948000
  assert.match(html, /原价开店 × 照常：约 76\.9 个月回本/);
  assert.match(html, /这套算法里没算进去的（1）/);
  assert.match(html, /每一格怎么算的/);
  assert.match(html, /客流 40\.00万/);
});

test('矩阵退化告警与未建模项都不藏', () => {
  const html = numbersReport({ok: true, cells: [], payback_months: {}, unmodeled: [],
    warning: ['两边动作互不影响：矩阵会退化，求解只能看出各自哪个动作更划算，说明不了博弈']},
    {me: '我', rival: '对方'});
  assert.match(html, /矩阵会退化/);
});

test('数值被转义，负利润读作负号加万元', () => {
  const html = numbersReport({ok: true,
    cells: [{me_move: '<img src=x>', rival_move: '照常', me_profit: -53520, rival_profit: 0,
             formula: {me: '<script>bad()</script>', rival: '—'}}],
    payback_months: {}, unmodeled: [], warning: []}, {me: '我', rival: '对方'});
  assert.ok(!html.includes('<img')); assert.ok(!html.includes('<script>'));
  assert.match(html, /−5\.4 万/);
});

test('推演台渲染出数字面板与全部字段', async () => {
  const {render} = await import('../js/views/playground.js');
  const nodes = new Map();
  const fake = (key) => {
    if (!nodes.has(key)) nodes.set(key, {innerHTML: '', textContent: '', value: '', style: {}, hidden: false,
      addEventListener() {}, querySelector: () => fake(key + ':child'), querySelectorAll: () => [],
      classList: {add() {}, remove() {}, toggle() {}}, setAttribute() {}, focus() {}, isConnected: true});
    return nodes.get(key);
  };
  const view = {innerHTML: '', addEventListener() {}, querySelector: (s) => fake(s), querySelectorAll: () => []};
  await render(view, {isCurrent: () => true});
  assert.match(view.innerHTML, /按数字摆局/);
  for (const id of ['traffic', 'share', 'price', 'margin', 'fixed', 'upfront', 'rshare', 'rprice', 'rfixed', 'cut', 'take', 'rtake']) {
    assert.match(view.innerHTML, new RegExp('id="pg-n-' + id + '"'), '缺少字段 ' + id);
  }
  assert.match(view.innerHTML, /市场总量不变/);
});

test('扫描报告说出生死线、不敏感项与口径', async () => {
  const {robustnessReport} = await import('../js/views/playground.js');
  const html = robustnessReport({
    ok: true,
    baseline: {equilibrium: '降价10% × 照常', me_profit: 125990},
    fragile: [{label: '区域月客流', scale: 0.7, equilibrium: '暂不开 × 照常'},
              {label: '我的每月固定成本', scale: 1.5, equilibrium: '暂不开 × 照常'}],
    checks: [{label: '区域月客流', flip: {scale: 0.7}},
             {label: '对方降价抢客力度', flip: null}],
  });
  assert.match(html, /结论的生死线/);
  assert.match(html, /降价10% × 照常/);
  assert.match(html, /区域月客流 跌到 70%/);
  assert.match(html, /我的每月固定成本 涨到 150%/);
  assert.match(html, /结论变成「暂不开 × 照常」/);
  assert.match(html, /扫了但没翻面的（1）/);
  assert.match(html, /只有纯策略均衡改变才算翻面/);
});

test('扫描失败或数字不全时不冒充结论', async () => {
  const {robustnessReport} = await import('../js/views/playground.js');
  const failed = robustnessReport({ok: false, error: '基线求解失败：超时'});
  assert.match(failed, /基线求解失败/);
  assert.ok(!failed.includes('生死线'));
  const gaps = robustnessReport({ok: false, error: '数字没填齐，先补上再扫', incomplete: ['我方的「margin」缺失或不合法']});
  assert.match(gaps, /数字没填齐/);
  assert.match(gaps, /我方的「margin」缺失或不合法/);
});

test('扫描入口渲染出按钮', async () => {
  const {robustnessPanel} = await import('../js/views/playground.js');
  const html = robustnessPanel();
  assert.match(html, /id="pg-robust"/);
  assert.match(html, /看看哪个假设最要命/);
});

function fakeView() {
  const nodes = new Map();
  const fake = (key) => {
    if (!nodes.has(key)) nodes.set(key, {innerHTML: '', textContent: '', value: '', style: {}, hidden: false,
      addEventListener() {}, querySelector: () => fake(key + ':child'), querySelectorAll: () => [],
      classList: {add() {}, remove() {}, toggle() {}}, setAttribute() {}, focus() {}, isConnected: true});
    return nodes.get(key);
  };
  return {nodes, view: {innerHTML: '', addEventListener() {}, querySelector: (s) => fake(s), querySelectorAll: () => []}};
}

test('存档入口渲染出按钮与说明', async () => {
  const {savePanel} = await import('../js/views/playground.js');
  const html = savePanel();
  assert.match(html, /id="pg-save"/);
  assert.match(html, /存成记录/);
  assert.match(html, /查找咨询/);
});

test('打开存档：数字回填、矩阵填回、标题与说明呈现', async () => {
  const {loadModel} = await import('../js/views/playground.js');
  const {api} = await import('../js/api.js');
  const {nodes, view} = fakeView();
  const saved = api.model;
  api.model = async () => ({model: {
    title: '我（喜茶） × 周边竞品 · 降价10% · 客流 40万', created_at: 1759000000,
    bundle: {market: {traffic: 400000},
             me: {name: '我（喜茶）', share: 0.06, price: 24, margin: 0.6, fixed_monthly: 330000, upfront: 1200000,
                  moves: [{}, {price_mult: 0.9, opponent_share_mult: 0.9}]},
             rival: {name: '周边竞品', share: 0.35, price: 22, margin: 0.6, fixed_monthly: 900000,
                     moves: [{}, {opponent_share_mult: 0.8}]}},
    spec: {strategies: {me: ['原价开店', '降价10%'], rival: ['照常', '降价跟进']}},
    cells: [{me_move: '原价开店', rival_move: '照常', me_profit: 15600, rival_profit: 948000,
             formula: {me: '客流 40万 × 份额 6%', rival: '对方算式'}}],
    payback_months: {}, unmodeled: [], warning: [], solution: null, robustness: null,
  }});
  try {
    await loadModel(view, 'model-abcdefabcdef');
  } finally { api.model = saved; }
  assert.equal(nodes.get('#pg-n-traffic').value, '400000');
  assert.equal(nodes.get('#pg-n-share').value, '6');
  assert.equal(nodes.get('#pg-n-cut').value, '10', '回填要收掉浮点噪声');
  assert.equal(nodes.get('#pg-n-take').value, '10');
  assert.equal(nodes.get('#pg-n-rtake').value, '20');
  assert.equal(nodes.get('#pg-p1-name').value, '我（喜茶）');
  const html = nodes.get('#pg-result').innerHTML;
  assert.match(html, /存档：我（喜茶） × 周边竞品/);
  assert.match(html, /按数字算出来的矩阵/);
  assert.match(html, /1\.6 万/);
  assert.equal(nodes.get('#pg-result-sub').textContent, '存档已打开');
});

test('存档打不开时如实说明，不留半个界面', async () => {
  const {loadModel} = await import('../js/views/playground.js');
  const {api} = await import('../js/api.js');
  const {nodes, view} = fakeView();
  const saved = api.model;
  api.model = async () => { throw new Error('没有这条记录。'); };
  try { await loadModel(view, 'model-abcdefabcdef'); } finally { api.model = saved; }
  assert.match(nodes.get('#pg-result').innerHTML, /这条存档打不开/);
  assert.match(nodes.get('#pg-result').innerHTML, /没有这条记录/);
});

test('查找咨询里能翻到数字摆局，且标注清楚不冒充咨询记录', async () => {
  const {mergeConsultations, filterConsultations} = await import('../js/views/cases.js');
  const rows = mergeConsultations([], [], [{id: 'model-abcdefabcdef', created_at: 1759000000,
    title: '我（喜茶） × 周边竞品 · 降价10%', me_profit: '125990', equilibria: 1, scanned: true}]);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].href, '#/playground/model-abcdefabcdef');
  assert.equal(rows[0].status, '数字摆局');
  assert.equal(rows[0].kind, 'model', '摆局按「做的事」归到数字摆局，不冒充经营分析');
  assert.match(rows[0].date, /^\d{4}-\d{2}-\d{2}$/);
  assert.equal(filterConsultations(rows, 'all', '喜茶').length, 1);
  assert.equal(filterConsultations(rows, 'all', '数字摆局').length, 1);
});

test('周边事实：计数、品牌存量、来源与口径都摆出来', async () => {
  const {poiReport} = await import('../js/views/playground.js');
  const html = poiReport({ok: true, address: '上海市黄浦区南京东路', radius: 500,
    source: '高德开放平台 Web 服务（POI 周边搜索）', retrieved_at: '2026-10-09T15:59:24+00:00',
    counts: [{label: '茶饮咖啡', count: 144, samples: ['瑞幸咖啡', '星巴克']},
             {label: '写字楼', count: 86, samples: []}],
    brand: {label: '喜茶（同品牌）', count: 3, samples: ['喜茶(甲店)']},
    unavailable: []});
  assert.match(html, /茶饮咖啡/); assert.match(html, /144/);
  assert.match(html, /瑞幸咖啡、星巴克/);
  assert.match(html, /喜茶（同品牌）：3 家/);
  assert.match(html, /同品牌已经开到这儿了/);
  assert.match(html, /高德开放平台/);
  assert.match(html, /不是营业额或客流/);
});

test('周边事实：查不到的项如实列出，不写 0', async () => {
  const {poiReport} = await import('../js/views/playground.js');
  const html = poiReport({ok: true, address: '某地', radius: 500, counts: [],
    unavailable: [{label: '餐饮', why: 'CUQPS_HAS_EXCEEDED_THE_LIMIT'}]});
  assert.match(html, /这次没查到的（1）/);
  assert.match(html, /CUQPS_HAS_EXCEEDED_THE_LIMIT/);
  assert.match(html, /没查到不等于没有/);
});

test('周边事实：真失败时只报原因，不给空表', async () => {
  const {poiReport} = await import('../js/views/playground.js');
  const html = poiReport({ok: false, error: '没有配置高德密钥'});
  assert.match(html, /没查到周边：没有配置高德密钥/);
  assert.ok(!html.includes('<table'));
});

test('存档里的周边事实标注「只留计数」', async () => {
  const {poiReport} = await import('../js/views/playground.js');
  const html = poiReport({ok: true, address: '某地', radius: 500, source: '高德',
    counts: [{label: '茶饮咖啡', count: 144}]});
  assert.match(html, /只留计数与口径/);
});

test('推演台渲染出查周边入口', async () => {
  const {render} = await import('../js/views/playground.js');
  const {view} = fakeView();
  await render(view, {isCurrent: () => true});
  assert.match(view.innerHTML, /id="pg-poi-address"/);
  assert.match(view.innerHTML, /id="pg-poi-run"/);
  assert.match(view.innerHTML, /先查周边（高德 POI）/);
});
