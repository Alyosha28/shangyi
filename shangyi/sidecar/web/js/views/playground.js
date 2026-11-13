// @ts-check
// 推演台：先让用户描述双方选择与相对结果，高级区保留原始矩阵与求解设置。
// 计算仍只走 `shangyi solve`；输入分数是情景假设，不是现实预测。
// 表单与卡片用 Tabler 组件（form-control / form-select / btn / card）。
import { api } from "../api.js";
import { esc, METHOD_LABEL, REFINE_LABEL } from "../util.js";
import { equilibriaCards, eqCellKeys, payoffMatrix } from "../components.js";

// ── 预设（数值经典 + 商业场景） ────────────────────────────────────────────
export const PRESETS = {
  prisoner: {
    label: "两人都怕对方先变卦",
    p1: { name: "嫌疑人甲", strategies: ["沉默", "招供"] },
    p2: { name: "嫌疑人乙", strategies: ["沉默", "招供"] },
    cells: { "沉默,沉默": [-1, -1], "沉默,招供": [-3, 0], "招供,沉默": [0, -3], "招供,招供": [-2, -2] },
  },
  price_war: {
    label: "别人降价，我怎么办？",
    p1: { name: "我", strategies: ["保持原价", "跟着降价"] },
    p2: { name: "另一家店", strategies: ["保持原价", "降价"] },
    cells: { "保持原价,保持原价": [1, 1], "保持原价,降价": [-1, 2], "跟着降价,保持原价": [2, -1], "跟着降价,降价": [0, 0] },
  },
  entry: {
    label: "要不要在附近开店？",
    p1: { name: "我", strategies: ["开店", "暂不开店"] },
    p2: { name: "附近的老店", strategies: ["打折揽客", "照常营业"] },
    cells: { "开店,打折揽客": [-2, 0], "开店,照常营业": [2, 1], "暂不开店,打折揽客": [0, 2], "暂不开店,照常营业": [0, 2] },
  },
  battle: {
    label: "两个人喜欢的活动不同",
    p1: { name: "甲方", strategies: ["足球", "电影"] },
    p2: { name: "乙方", strategies: ["足球", "电影"] },
    cells: { "足球,足球": [3, 1], "足球,电影": [0, 0], "电影,足球": [0, 0], "电影,电影": [1, 3] },
  },
  chicken: {
    label: "两个人都不愿先让",
    p1: { name: "少年A", strategies: ["转向", "直行"] },
    p2: { name: "少年B", strategies: ["转向", "直行"] },
    cells: { "转向,转向": [0, 0], "转向,直行": [-1, 2], "直行,转向": [2, -1], "直行,直行": [-10, -10] },
  },
};

const DEFAULT = PRESETS.price_war;
export const FEELINGS=[
  {value:-2,label:'很吃亏'}, {value:-1,label:'有点吃亏'},
  {value:0,label:'差不多'}, {value:1,label:'有点划算'}, {value:2,label:'很划算'}
];

// ── 数字面板：把事实数字编译成矩阵（收益由算式生成，不是手填） ──────────────
// 假设写在界面上：市场总量不变（抢来的客来自对方）；单位成本不变（降价后毛利率自动重算）。
const NUM_FIELDS = [
  ["traffic", "区域月客流", "人", 400000],
  ["share", "我能拿到的份额", "%", 6],
  ["price", "我的客单价", "元", 24],
  ["margin", "我的毛利率", "%", 60],
  ["fixed", "我每月固定成本", "元", 330000],
  ["upfront", "我的一次性投入", "元", 1200000],
  ["rshare", "对方份额", "%", 35],
  ["rprice", "对方客单价", "元", 22],
  ["rfixed", "对方每月固定成本", "元", 900000],
  ["cut", "降价幅度（双方同幅）", "%", 10],
  ["take", "我降价能抢走对方客流", "%", 10],
  ["rtake", "对方降价会抢走我客流", "%", 20],
];

function numbersHtml() {
  return `<details class="pg-numbers"><summary>有数字？按数字摆局</summary>
    <p class="pg-step-help">填你手上有的数；没有的先填估计值，它会被当作假设。生成的每一格都带算式，可核对。</p>
    <div class="pg-poi">
      <div class="pg-poi-row">
        <label class="pg-poi-addr"><span class="pg-n-label">先查周边（高德 POI）</span>
          <input class="form-control form-control-sm" id="pg-poi-address" placeholder="例如：上海市南京东路32号"></label>
        <label><span class="pg-n-label">半径</span><span class="pg-n-input"><input type="number" class="form-control form-control-sm" id="pg-poi-radius" value="500"><span class="pg-unit">米</span></span></label>
        <label><span class="pg-n-label">同品牌</span><input class="form-control form-control-sm" id="pg-poi-brand" placeholder="例如：喜茶"></label>
        <button class="btn btn-sm" id="pg-poi-run">查周边</button>
      </div>
      <div id="pg-poi-result"></div>
    </div>
    <div class="pg-number-grid">${NUM_FIELDS.map(([id, label, unit, value]) =>
      `<label><span class="pg-n-label">${label}</span><span class="pg-n-input"><input type="number" class="form-control form-control-sm" id="pg-n-${id}" value="${value}"><span class="pg-unit">${unit}</span></span></label>`).join("")}</div>
    <div class="pg-number-actions"><button class="btn btn-primary btn-sm" id="pg-numbers-run">按数字摆局</button>
    <span class="small-note">假设：市场总量不变；单位成本不变（降价后毛利率自动重算）。</span></div>
  </details>`;
}

function numValue(view, id) {
  const field = view.querySelector("#pg-n-" + id);
  const value = Number(field?.value);
  return Number.isFinite(value) ? value : NaN;
}

/** 面板数字 → /api/model 请求体。抢客影响按「市场总量不变」换算成双方份额变动。 */
export function numbersBundle(view, names) {
  const traffic = numValue(view, "traffic"), share = numValue(view, "share") / 100,
    price = numValue(view, "price"), margin = numValue(view, "margin") / 100,
    fixed = numValue(view, "fixed"), upfront = numValue(view, "upfront"),
    rshare = numValue(view, "rshare") / 100, rprice = numValue(view, "rprice"),
    rfixed = numValue(view, "rfixed"), cut = numValue(view, "cut") / 100,
    take = numValue(view, "take") / 100, rtake = numValue(view, "rtake") / 100;
  const discount = "降价" + Math.round(cut * 100) + "%";
  return {
    market: { traffic },
    me: {
      name: names.me, share, price, margin, fixed_monthly: fixed, upfront,
      moves: [
        { name: "原价开店", share_mult: 1, price_mult: 1 },
        // 我抢来的绝对客流 = 对方份额 × 抢走比例 ⇒ 换算成我的份额乘数；对方份额同步减少。
        { name: discount, price_mult: 1 - cut,
          share_mult: 1 + (rshare * take) / share, opponent_share_mult: 1 - take },
        { name: "暂不开", closed: true },
      ],
    },
    rival: {
      name: names.rival, share: rshare, price: rprice, margin, fixed_monthly: rfixed,
      moves: [
        { name: "照常", share_mult: 1, price_mult: 1 },
        { name: "降价跟进", price_mult: 1 - cut,
          share_mult: 1 + (share * rtake) / rshare, opponent_share_mult: 1 - rtake },
      ],
    },
  };
}

/** 编译结果 → 结果卡（算式、回本、未建模与告警都摆在明处）。 */
export function numbersReport(result, names) {
  if (!result?.ok) {
    const items = (result?.incomplete || []).map((x) => '<li>' + esc(x) + '</li>').join('');
    return '<div class="empty"><p class="empty-title text-danger">还差些数字，先补上</p>' +
      '<ul class="pg-gap-list">' + items + '</ul>' +
      '<p class="empty-subtitle">这些是算矩阵必需的；不补就不算，免得得到一个看着像结论的数。</p></div>';
  }
  const rows = result.cells.map((c) =>
    '<tr><td>' + esc(c.me_move) + ' × ' + esc(c.rival_move) + '</td>' +
    '<td class="text-end">' + fmtMoney(c.me_profit) + '</td>' +
    '<td class="text-end">' + fmtMoney(c.rival_profit) + '</td></tr>').join('');
  const payback = Object.entries(result.payback_months || {})
    .map(([k, v]) => '<li>' + esc(k.replace(',', ' × ')) + '：约 ' + v + ' 个月回本</li>').join('');
  const flags = [...(result.warning || []), ...(result.unmodeled || [])];
  const flagsHtml = flags.length
    ? '<details class="pg-flags"><summary>这套算法里没算进去的（' + flags.length + '）</summary><ul>' +
      flags.map((f) => '<li>' + esc(f) + '</li>').join('') +
      '</ul><p class="small-note">未声明不等于没影响，只是这次没建模。</p></details>'
    : '';
  return '<div class="pg-model-report"><h3>按数字算出来的矩阵</h3>' +
    '<table class="table table-sm pg-model-table"><thead><tr><th>情况</th>' +
    '<th class="text-end">我（' + esc(names.me) + '）</th>' +
    '<th class="text-end">对方（' + esc(names.rival) + '）</th></tr></thead><tbody>' + rows + '</tbody></table>' +
    '<p class="small-note">单位：元/月。矩阵已填进下面的表，点“看看结果”让引擎算均衡。</p>' +
    (payback ? '<ul class="pg-payback">' + payback + '</ul>' : '') +
    flagsHtml +
    '<details class="pg-flags"><summary>每一格怎么算的</summary><ul>' +
    result.cells.map((c) => '<li><strong>' + esc(c.me_move) + ' × ' + esc(c.rival_move) + '</strong><br>我：' +
      esc(c.formula.me) + '<br>对方：' + esc(c.formula.rival) + '</li>').join('') +
    '</ul></details></div>';
}

/** 周边事实渲染。计数是硬事实；拿不到的项如实列出，不写 0。 */
export function poiReport(result) {
  if (!result?.ok) {
    return '<p class="small-note text-danger">没查到周边：' + esc(result?.error || '未取得') + '</p>';
  }
  const rows = (result.counts || []).map((c) =>
    '<tr><td>' + esc(c.label) + '</td><td class="text-end">' + esc(String(c.count)) + '</td>' +
    '<td class="pg-poi-samples">' + esc((c.samples || []).join('、') || '—') + '</td></tr>').join('');
  const brand = result.brand
    ? '<p class="small-note"><strong>' + esc(result.brand.label) + '：' + esc(String(result.brand.count)) +
      ' 家</strong>——同品牌已经开到这儿了，这件事直接影响你要不要进。</p>'
    : '';
  const missing = (result.unavailable || []).length
    ? '<details class="pg-flags"><summary>这次没查到的（' + result.unavailable.length + '）</summary><ul>' +
      result.unavailable.map((u) => '<li>' + esc(u.label) + '：' + esc(u.why) + '</li>').join('') +
      '</ul><p class="small-note">没查到不等于没有，只是这次没拿到。</p></details>'
    : '';
  const slim = !result.counts?.some((c) => c.samples);
  return '<div class="pg-poi-facts">' +
    '<table class="table table-sm pg-model-table"><thead><tr><th>周边</th><th class="text-end">数量</th><th>例</th></tr></thead><tbody>' +
    rows + '</tbody></table>' + brand +
    '<p class="small-note">' + esc(result.address || '') + ' · 半径 ' + esc(String(result.radius || '')) + ' 米 · ' +
    esc(result.source || '高德') + (result.retrieved_at ? ' · ' + esc(String(result.retrieved_at).slice(0, 16).replace('T', ' ')) : '') + '</p>' +
    (slim ? '<p class="small-note">存档里只留计数与口径，不留 POI 明细（高德条款）。</p>'
          : '<p class="small-note">计数是「这个半径内该类目的门店数量」，不是营业额或客流：用它校准你的份额与抢客假设。</p>') +
    missing + '</div>';
}

async function runPoi(view) {
  const button = view.querySelector('#pg-poi-run');
  const host = view.querySelector('#pg-poi-result');
  const address = String(view.querySelector('#pg-poi-address')?.value || '').trim();
  if (!address) {
    host.innerHTML = '<p class="small-note text-danger">先填一个地址，例如「上海市南京东路32号」。</p>';
    return;
  }
  button.disabled = true;
  button.textContent = '正在查…';
  host.innerHTML = '<p class="small-note">正在逐类目查询（免费 key 有限流，会慢一点）…</p>';
  try {
    lastPoi = await api.poi({address,
      radius: Number(view.querySelector('#pg-poi-radius')?.value) || 500,
      brand: String(view.querySelector('#pg-poi-brand')?.value || '').trim()});
    host.innerHTML = poiReport(lastPoi);
  } catch (error) {
    lastPoi = null;
    host.innerHTML = '<p class="small-note text-danger">没查到周边：' + esc(error.message) + '</p>';
  } finally {
    button.disabled = false;
    button.textContent = '查周边';
  }
}

/** 存档入口：把这次摆的局存进「查找咨询」，以后能翻出来。 */
export function savePanel() {
  return '<div class="pg-save"><button class="btn btn-sm" id="pg-save">存成记录</button>' +
    '<span class="small-note" id="pg-save-note">存进「查找咨询」，以后能翻出来对</span></div>';
}

async function saveModel(view, names) {
  const button = view.querySelector('#pg-save');
  const note = view.querySelector('#pg-save-note');
  button.disabled = true;
  button.textContent = '正在存…';
  try {
    const saved = await api.saveModel({bundle: numbersBundle(view, names),
      robustness: Boolean(lastRobustness), poi: lastPoi || undefined});
    note.textContent = '已存好：' + saved.title + '（在「查找咨询」里能找到）';
    window.dispatchEvent(new Event('shangyi:history'));
  } catch (error) {
    note.textContent = '没存上：' + error.message;
  } finally {
    button.disabled = false;
    button.textContent = '存成记录';
  }
}

/** 打开一条存档：把面板数字还原、矩阵填回、结果与扫描一并呈现。 */
export async function loadModel(view, id) {
  const result = view.querySelector('#pg-result');
  try {
    const {model} = await api.model(id);
    applyBundle(view, model.bundle || {});
    if (model.spec?.strategies) {
      state.p1.strategies = [...model.spec.strategies.me];
      state.p2.strategies = [...model.spec.strategies.rival];
    }
    state.cells = {};
    // 存档来自「按数字摆局」：数字是用户自己填的假设，不是示例判断。
    state.origin = {};
    for (const c of model.cells || []) state.cells[c.me_move + "," + c.rival_move] = [c.me_profit, c.rival_profit];
    lastSol = model.solution || null;
    lastRobustness = model.robustness || null;
    lastPoi = model.facts?.counts?.length ? {...model.facts, ok: true} : null;
    const poiHost = view.querySelector('#pg-poi-result');
    if (poiHost) poiHost.innerHTML = lastPoi ? poiReport(lastPoi) : '';
    revision++;
    drawChoices(view); drawOutcomes(view); drawMatrix(view); drawSpec(view);
    const names = {me: state.p1.name, rival: state.p2.name};
    const when = model.created_at ? new Date(model.created_at * 1000).toLocaleString('zh-CN') : '';
    result.innerHTML = '<p class="small-note">存档：' + esc(model.title || '数字摆局') + (when ? '（' + esc(when) + '）' : '') + '</p>' +
      (lastSol ? explainSolution(lastSol, state.p1, state.p2, pendingExamples()) : '') +
      numbersReport({ok: true, cells: model.cells || [], payback_months: model.payback_months || {},
                     unmodeled: model.unmodeled || [], warning: model.warning || []}, names) +
      (lastRobustness ? robustnessReport(lastRobustness) : '') +
      (model.facts?.counts?.length ? poiReport({...model.facts, ok: true}) : '');
    view.querySelector('#pg-result-technical').innerHTML = lastSol
      ? equilibriaCards(lastSol, strategiesOf(), {[state.p1.name]: state.p1.name, [state.p2.name]: state.p2.name})
      : '';
    view.querySelector('#pg-result-sub').textContent = '存档已打开';
  } catch (error) {
    result.innerHTML = '<p class="text-danger">这条存档打不开：' + esc(error.message) + '</p>';
    view.querySelector('#pg-result-sub').textContent = '存档未打开';
  }
}

/** 存档里的数字还原回面板（与 numbersBundle 一一对应）。 */
function applyBundle(view, bundle) {
  const set = (id, value) => {
    const field = view.querySelector('#pg-n-' + id);
    const number = Number(value);
    if (!field || !Number.isFinite(number)) return;
    // 收一下精度：1−0.9 会算出 9.999999999999998，直接写进输入框太难看了。
    field.value = String(Math.round(number * 1e6) / 1e6);
  };
  const me = bundle.me || {}, rival = bundle.rival || {}, market = bundle.market || {};
  const myMoves = me.moves || [], rivalMoves = rival.moves || [];
  set('traffic', market.traffic);
  set('share', Number(me.share) * 100);
  set('price', me.price);
  set('margin', Number(me.margin) * 100);
  set('fixed', me.fixed_monthly);
  set('upfront', me.upfront);
  set('rshare', Number(rival.share) * 100);
  set('rprice', rival.price);
  set('rfixed', rival.fixed_monthly);
  if (myMoves[1]?.price_mult != null) set('cut', (1 - myMoves[1].price_mult) * 100);
  if (myMoves[1]?.opponent_share_mult != null) set('take', (1 - myMoves[1].opponent_share_mult) * 100);
  if (rivalMoves[1]?.opponent_share_mult != null) set('rtake', (1 - rivalMoves[1].opponent_share_mult) * 100);
  if (me.name) { state.p1.name = me.name; const field = view.querySelector('#pg-p1-name'); if (field) field.value = me.name; }
  if (rival.name) { state.p2.name = rival.name; const field = view.querySelector('#pg-p2-name'); if (field) field.value = rival.name; }
}

/** 扫描入口：按同一份数字逐个推偏假设。 */
export function robustnessPanel() {
  return '<div class="pg-robustness"><button class="btn btn-sm" id="pg-robust">看看哪个假设最要命</button>' +
    '<div id="pg-robust-result"></div></div>';
}

function scaleWords(scale) {
  const percent = Math.round(scale * 100);
  return scale < 1 ? '跌到 ' + percent + '%' : scale > 1 ? '涨到 ' + percent + '%' : '不变';
}

/** 扫描结果 → 生死线 / 不敏感 / 口径，三段都说清楚。 */
export function robustnessReport(report) {
  if (!report?.ok) {
    const gaps = (report?.incomplete || []).map((x) => '<li>' + esc(x) + '</li>').join('');
    return '<p class="text-danger">' + esc(report?.error || '扫描没成功') + '</p>' +
      (gaps ? '<ul class="pg-gap-list">' + gaps + '</ul>' : '');
  }
  const base = report.baseline;
  const fragile = (report.fragile || []).map((f) =>
    '<li><strong>' + esc(f.label) + ' ' + scaleWords(f.scale) + '</strong> → 结论变成「' +
    esc(f.equilibrium) + '」</li>').join('');
  const stableLabels = (report.checks || []).filter((c) => !c.flip).map((c) => '<li>' + esc(c.label) + '</li>').join('');
  const stable = stableLabels
    ? '<details class="pg-flags"><summary>扫了但没翻面的（' + (report.checks || []).filter((c) => !c.flip).length + '）</summary>' +
      '<ul>' + stableLabels + '</ul><p class="small-note">在扫描范围内，这些假设怎么变都不改变结论。</p></details>'
    : '';
  return '<div class="pg-model-report"><h3>结论的生死线</h3>' +
    '<p class="small-note">当前结论：<strong>' + esc(base.equilibrium) + '</strong>，我每月约 ' +
    fmtMoney(base.me_profit) + '。下面是把每个假设按比例推偏后，结论开始翻面的位置。</p>' +
    (fragile ? '<ul class="pg-fragile">' + fragile + '</ul>'
             : '<p class="small-note">在扫描范围内没有假设能让结论翻面。</p>') +
    stable +
    '<p class="small-note">口径：只有纯策略均衡改变才算翻面；收益随参数连续变化，不叫翻面。扫描按由轻到重的比例逐个试，回报最靠近原值的那一次。</p>' +
    '</div>';
}

async function runRobustness(view, names) {
  const button = view.querySelector('#pg-robust');
  const host = view.querySelector('#pg-robust-result');
  button.disabled = true;
  button.textContent = '正在逐个推偏假设…';
  host.innerHTML = '<p class="small-note">正在重编译、重求解…</p>';
  try {
    lastRobustness = await api.robustness({bundle: numbersBundle(view, names)});
    host.innerHTML = robustnessReport(lastRobustness);
  } catch (error) {
    host.innerHTML = '<p class="text-danger">扫描没成功：' + esc(error.message) + '</p>';
  } finally {
    button.disabled = false;
    button.textContent = '看看哪个假设最要命';
  }
}

function fmtMoney(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return "—";
  const sign = n < 0 ? "−" : "";
  const abs = Math.abs(n);
  if (abs >= 10000) return sign + (abs / 10000).toFixed(1).replace(/\.0$/, "") + " 万";
  return sign + Math.round(abs).toLocaleString("zh-CN");
}

let state = null;
let lastSol = null;
let lastRobustness = null;
let lastPoi = null;
let revision = 0;

/** 由一份预设格子算出初始来源标记：有值的格子都算「还是示例的判断」。 */
function originOf(cells){
 const origin={};
 for(const [key,cell] of Object.entries(cells)){
  origin[key]=[cell?.[0]!=null&&cell[0]!=='',cell?.[1]!=null&&cell[1]!==''];
 }
 return origin;
}

/** 新建一份「刚点开某个示例」的状态。纯函数：界面初始化与测试走同一份逻辑。 */
export function freshExampleState(preset){
 return {
  p1:structuredClone(preset.p1),
  p2:structuredClone(preset.p2),
  cells:structuredClone(preset.cells),
  // origin[key] = [这一格我方的好坏是否仍来自示例, 对方是否仍来自示例]。
  // 例子的好坏**不是用户自己的判断**：用户没核对过，结果里就不能那么称呼它
  // （UI-009：改个名字就静默继承示例好坏，结论还写着「这些好坏是你自己选的」）。
  origin:originOf(structuredClone(preset.cells)),
  method:'support_enumeration',
  refine:'',
  refineDen:6,
 };
}

function initState(preset) {
  revision++;
  state = freshExampleState(preset);
  lastSol = null;
}

// ── 草稿：刷新不该让用户重填一遍（UI-002）──────────────────────────────────
// 原来 state 只在模块内存里，一刷新就回默认示例，用户改过的名称/做法/好坏
// 全丢，而且**没有任何提示**。这里按仓库已有范式（run.js / chat.js 的开场草稿）
// 存进 sessionStorage：同标签页刷新后恢复，存不进去就明说，不冒称已保存。
// 注意**不恢复结果**——上一轮的计算对不上恢复后的输入，恢复后明确要求重算。

const DRAFT_KEY = 'shangyi-playground-draft-v1';
const DRAFT_DEFAULTS = { method: 'support_enumeration', refine: '', refineDen: 6 };

/** 草稿形状校验：不合形状就当没有，不猜、不补。 */
export function validDraft(value) {
  if (!value || typeof value !== 'object') return false;
  for (const side of [value.p1, value.p2]) {
    if (!side || typeof side.name !== 'string' || !side.name.trim()) return false;
    if (!Array.isArray(side.strategies) || side.strategies.length < 2 || side.strategies.length > 4) return false;
    if (!side.strategies.every((s) => typeof s === 'string' && s.trim())) return false;
  }
  if (!value.cells || typeof value.cells !== 'object') return false;
  if (value.origin != null && typeof value.origin !== 'object') return false;
  if (typeof value.method === 'string' && !METHOD_LABEL[value.method]) return false;
  if (typeof value.refine === 'string' && value.refine !== '' && !REFINE_LABEL[value.refine]) return false;
  return true;
}

/** 解析存储里的原文。坏 JSON、空串、非字符串一律当没有草稿。 */
export function parseDraft(raw) {
  if (typeof raw !== 'string' || !raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

export function readDraft() {
  try {
    return parseDraft(sessionStorage.getItem(DRAFT_KEY));
  } catch {
    return null;
  }
}

/** 由草稿造出一份可用状态；形状不对返回 null（调用方就当没有草稿）。 */
export function stateFromDraft(draft) {
  if (!validDraft(draft)) return null;
  return {
    p1: structuredClone(draft.p1),
    p2: structuredClone(draft.p2),
    cells: structuredClone(draft.cells),
    origin: draft.origin ? structuredClone(draft.origin) : {},
    method: typeof draft.method === 'string' ? draft.method : DRAFT_DEFAULTS.method,
    refine: typeof draft.refine === 'string' ? draft.refine : DRAFT_DEFAULTS.refine,
    // 夹在合法区间：超界的档位会把 9999 这类数字直接传给求解器
    refineDen: Number.isFinite(draft.refineDen)
      ? Math.min(64, Math.max(2, Math.round(draft.refineDen)))
      : DRAFT_DEFAULTS.refineDen,
  };
}

/** 把草稿盖到当前 state 上；形状不对就不动，返回是否真的恢复了。 */
export function applyDraft(draft) {
  const next = stateFromDraft(draft);
  if (!next) return false;
  state = next;
  return true;
}

/** 当前模块状态。测试需要读它——渲染路径走真实 DOM，测试没有 DOM。 */
export function currentState(){return state;}

/** 要写进存储的原文。拆出来是为了能测「存进去的再读回来还对得上」。 */
export function draftJson(current) {
  return JSON.stringify({
    p1: current.p1,
    p2: current.p2,
    cells: current.cells,
    origin: current.origin,
    method: current.method,
    refine: current.refine,
    refineDen: current.refineDen,
  });
}

/** 写盘。返回 false 表示没存上（隐私模式/配额）——调用方要让人知道。 */
function writeDraft() {
  try {
    sessionStorage.setItem(DRAFT_KEY, draftJson(state));
    return true;
  } catch {
    return false;
  }
}

let draftWarning = false;
let choiceErrorSeq = 0;
let currentPresetKey = 'price_war';   // 与 DEFAULT（PRESETS.price_war）一致
/** 提示条节点。测试环境可能连 document 都没有——那时没有可提示的地方，但不该因此崩。 */
function draftNoteEl() {
  if (typeof document === 'undefined' || typeof document.getElementById !== 'function') return null;
  return document.getElementById('pg-draft-warning');
}
function persistDraft() {
  if (state === null) return;
  const ok = writeDraft();
  if (ok === draftWarning) {
    draftWarning = !ok;
    const note = draftNoteEl();
    if (note) {
      note.hidden = !draftWarning;
      note.textContent = draftWarning
        ? '这次的编辑没能存下来（浏览器不允许本地存储）。内容还在页面上，但刷新就没了；能恢复存储后再刷新。'
        : '';
    }
  }
}


/** 还没填好的格子：具体到「我方做法 / 对方做法 / 哪一方还不知道」。
 *
 *  UI-011：原来只给一句通用提示「每种情况都要选出双方觉得好还是吃亏」，
 *  组合多的时候用户得逐项找空缺，也没有回到那个字段的入口。
 *  这里把空缺列出来，并给每个空缺一个能跳回去的锚点。*/
export function unknownCells(current){
 const out=[];
 for(let i=0;i<current.p1.strategies.length;i++)for(let j=0;j<current.p2.strategies.length;j++){
  const cell=current.cells?.[current.p1.strategies[i]+','+current.p2.strategies[j]]||['',''];
  const missing=[];
  if(cell[0]==null||cell[0]==='')missing.push(0);
  if(cell[1]==null||cell[1]==='')missing.push(1);
  if(missing.length)out.push({i,j,missing,
   label:current.p1.strategies[i]+'／'+current.p2.strategies[j],
   sides:missing.map(k=>k===0?current.p1.name:current.p2.name)});
 }
 return out;
}

/** 未填齐时结果区怎么说话：列清楚是哪几种组合、哪一方，并给回到字段的指引。*/
export function incompleteNotice(current){
 const unknown=unknownCells(current);
 if(!unknown.length)return '';
 const names=new Set(unknown.flatMap(x=>x.sides));
 const list=unknown.map(x=>'<li><a href="#pg-cell-'+x.i+'-'+x.j+'">'+esc(x.label)+'</a>'
  +'（'+esc(x.sides.join('、'))+'还不知道）</li>').join('');
 return '<div class="empty"><p class="empty-title">还有 '+unknown.length+' 种情况没选完</p>'+
  '<p class="empty-subtitle">下面这些组合里，'+esc([...names].join('、'))+'觉得好还是吃亏还没选。选「还不知道」的地方会先搁着，不替你猜；下面每一条都能点回去改。</p>'+
  '<ul class="pg-unknown-list">'+list+'</ul>'+
  '<p class="small-note">确实没有这方面的信息的，可以先放着——改天知道了再回来补，或者直接问对方。</p></div>';
}

export async function render(view, ctx) {
  let restored = false;
  if (!state) {
    initState(DEFAULT);
    const draft = readDraft();
    if (draft) restored = applyDraft(draft);
  }
  view.innerHTML = `
  <div class="pg-intro"><h1>换个做法，结果会怎样？</h1><p>先选一个例子，再把双方的做法改成你的情况。接着想一想：如果双方都这样做，谁会吃亏，谁会觉得划算？</p><p class="pg-caveat">这是你自己试填的“如果……会怎样”；不查真利润，也不猜对方下一步。</p></div>
  <div class="row row-cards pg-workspace">
    <div class="col-lg-7">
      <div class="card">
        <div class="card-header">
          <h2 class="card-title">先选一个例子</h2>
        </div>
        <div class="card-body">
          <div class="chip-row pg-presets" aria-label="示例场景">
            <button type="button" class="btn btn-outline-secondary btn-sm" data-preset="price_war">别人降价，我怎么办？</button>
            <button type="button" class="btn btn-outline-secondary btn-sm" data-preset="entry">要不要在附近开店？</button>
          </div>
          <p class="pg-draft-warning" id="pg-draft-warning" role="status" hidden></p>
          <div class="pg-preset-confirm" id="pg-preset-confirm" role="alertdialog" aria-label="切换示例前确认" hidden></div>
          <p class="pg-example-note">先点一个例子看看；例子里的好坏只是演示。改成你的情况后，原来那套好坏还会在，逐个核对过才算你的判断。</p>
          ${numbersHtml()}
          <div class="row g-2 mb-3">
            <div class="col">
              <label class="form-label" for="pg-p1-name">你这边叫什么</label>
              <input type="text" class="form-control form-control-sm" id="pg-p1-name">
            </div>
            <div class="col">
              <label class="form-label" for="pg-p2-name">另一边叫什么</label>
              <input type="text" class="form-control form-control-sm" id="pg-p2-name">
            </div>
          </div>
          <h3 class="pg-step-title">再写双方会怎么做</h3>
          <p class="pg-step-help">你和对方各填两种可能的做法。不确定对方会怎样也没关系，这里只是先试着比较。</p>
          <div id="pg-choices"></div>
          <h3 class="pg-step-title">最后选每种情况的好坏</h3>
          <p class="pg-step-help">不用填数字。分别选你觉得怎样、对方会怎样。不清楚就选“还不知道”；有不知道的会先搁着，不替你猜。</p>
          <div id="pg-outcomes"></div>
          <button class="btn btn-primary pg-main-action" id="pg-solve">看看结果</button>
          <details class="pg-advanced"><summary>看数字和设置</summary>
            <p class="pg-step-help">上面五种好坏，在这儿记作 -2、-1、0、1、2。数字只比先后，不代表钱。下面还有给懂行的人用的详细设置。</p>
            <div class="chip-row">
              ${Object.entries(PRESETS)
                .filter(([k])=>!['price_war','entry'].includes(k))
                .map(([k,p])=>`<button type="button" class="btn btn-outline-secondary btn-sm" data-preset="${k}">${esc(p.label)}（教学示例）</button>`).join('')}
            </div>
            <div class="pg-advanced-actions"><button class="btn btn-sm" id="pg-add-strat">各加一种</button><button class="btn btn-sm" id="pg-del-strat">各减一种</button></div>
            <div id="pg-matrix"></div>
            <div class="text-secondary small mt-2">表里是上面好坏对应的数字；每边能填 2—4 种。</div>
          <details class="pg-expert"><summary>更多计算选项（平时不用改）</summary>
          <div class="row g-2 mt-1 align-items-end">
            <div class="col">
              <label class="form-label" for="pg-method">计算方式</label>
              <select class="form-select form-select-sm" id="pg-method">
                ${Object.entries(METHOD_LABEL)
                  .map(([v, l]) => `<option value="${v}">${esc(l)}</option>`)
                  .join("")}
              </select>
            </div>
            <div class="col">
              <label class="form-label" for="pg-refine">挑结果（可选）</label>
              <select class="form-select form-select-sm" id="pg-refine">
                <option value="">不挑，先看全部</option>
                ${Object.entries(REFINE_LABEL)
                  .map(([v, l]) => `<option value="${v}">${esc(l)}</option>`)
                  .join("")}
              </select>
            </div>
            <div class="col" id="pg-den-field" style="display:none">
              <label class="form-label" for="pg-den">算多细</label>
              <input type="number" class="form-control form-control-sm" id="pg-den" min="2" max="64" value="6">
            </div>
          </div>
          <details class="raw" id="pg-spec-raw"><summary>看系统收到啥</summary><pre id="pg-spec-pre"></pre></details>
          </details>
          </details>
        </div>
      </div>
    </div>

    <div class="col-lg-5">
      <div class="card" id="pg-result-card" tabindex="-1" aria-label="比较结果">
        <div class="card-header">
          <h2 class="card-title">算出来的情况</h2>
          <div class="card-actions"><span class="text-secondary small" id="pg-result-sub">还没开始</span></div>
        </div>
        <div class="card-body" id="pg-result" aria-live="polite">
          <div class="empty"><p class="empty-title">${restored?'上次填的还在':'先选一个例子'}</p><p class="empty-subtitle">${restored?'条件已恢复；点“看看结果”重新算一次。':'改好两边做法和好坏，再点“看看结果”。'}</p></div>
        </div>
        <details class="pg-result-detail"><summary>看详细算法</summary><div id="pg-result-technical"></div><div id="pg-result-matrix"></div></details>
      </div>
    </div>
  </div>`;

  draftWarning = false;
  bindPresets(view);
  bindControls(view);
  bindNumbers(view);
  view.addEventListener("input", () => invalidate(view));
  view.addEventListener("change", () => invalidate(view));
  drawMatrix(view);
  drawChoices(view);
  drawOutcomes(view);
  drawSpec(view);
  if (restored) {
    view.querySelector('#pg-result-sub').textContent = '条件已恢复，待重新计算';
  }
  const savedId = ctx.match?.[1];
  if (savedId) await loadModel(view, savedId);
}

// ── 矩阵编辑器 ──────────────────────────────────────────────────────────────


/** 改名后把界面上依赖做法名称的零散位置同步一遍（不整块重绘，避免抢焦点）。*/
function syncRenamedLabels(view){
 const matrix=view.querySelector('#pg-matrix');
 if(matrix){
  matrix.querySelectorAll('.strat-name').forEach(inp=>{
   const side=+inp.dataset.side,i=+inp.dataset.i;
   const list=side===1?state.p1.strategies:state.p2.strategies;
   if(list[i]!==inp.value&&document.activeElement!==inp)inp.value=list[i];
  });
 }
 const outcomes=view.querySelector('#pg-outcomes');
 if(outcomes){
  const sections=outcomes.querySelectorAll('.pg-outcome');
  let idx=0;
  for(let i=0;i<state.p1.strategies.length;i++)for(let j=0;j<state.p2.strategies.length;j++){
   const section=sections[idx++];
   const head=section?.querySelector('h4');
   if(head)head.innerHTML=esc(state.p1.name)+'：'+esc(state.p1.strategies[i])+'<br>'+esc(state.p2.name)+'：'+esc(state.p2.strategies[j]);
  }
 }
}

/** 重绘后把焦点还给原来那个输入框（连同光标位置）。*/
function restoreFocus(view,key,inp){
 const target=view.querySelector('.pg-choice-name[data-side="'+key.split(':')[0]+'"][data-i="'+key.split(':')[1]+'"]')
  ||view.querySelector('.strat-name[data-side="'+key.split(':')[0]+'"][data-i="'+key.split(':')[1]+'"]')
  ||inp;
 if(document.activeElement!==target&&document.activeElement===document.body){
  target.focus({preventScroll:true});
  const caret=Number(key.split(':')[2]);
  if(Number.isFinite(caret)&&target.setSelectionRange)target.setSelectionRange(caret,caret);
 }
}

/** 做法名称的两项校验：英文逗号、同一边重名。**分开判定、分开提示**——
 *  UI-007：原来合成一个分支，用户写「保持原价,送优惠券」被回复「不能重名」，
 *  照着"避免重复"去改还是被拒，因为真正的原因是逗号（它是矩阵 cell 的键分隔符）。
 *  提示改到字段旁（字段附近可见 + aria 关联给读屏软件），并且**保留用户输入**，
 *  让他按提示改一个标点就行，不用重新打字。*/
export function validateChoiceName({name,others}){
 const trimmed=name.trim();
 if(!trimmed)return {ok:false,reason:'empty',message:'做法不能空着。'};
 if(trimmed.includes(','))return {ok:false,reason:'comma',
  message:'名称里不能有英文逗号（,）。请改用中文逗号或顿号，例如「保持原价，送优惠券」。'};
 if(others.some(other=>other.trim()===trimmed))return {ok:false,reason:'duplicate',
  message:'同一边的做法不能写成一样。请换一个说法。'};
 return {ok:true,value:trimmed};
}

export function updateChoiceName(view,side,i,inp){
 const choices=side==='1'?state.p1.strategies:state.p2.strategies;
 const old=choices[i];
 const verdict=validateChoiceName({name:inp.value,others:choices.filter((_,index)=>index!==i)});
 if(!verdict.ok){
  // **不写回旧值**：用户输入原样留着，只提示要改什么
  showChoiceError(view,inp,verdict.message);
  return;
 }
 if(verdict.value===old)return;
 inp.value=verdict.value;
 clearChoiceError(view,inp);
 const focusKey=side+':'+i+':'+inp.selectionStart;
 renameChoiceInState(state,side,i,verdict.value);
 // **不整块重绘**：innerHTML 重建会移除当前输入框，Tab 就落到 BODY。
 // 只同步这一处依赖名称的地方（矩阵表头/行头、格子标题），并保持其它字段不动。
 syncRenamedLabels(view);
 drawOutcomes(view);drawSpec(view);
 restoreFocus(view,focusKey,inp);
}

/** 校验提示挂在字段旁边，用 aria 关联，读屏能念到。 */
function showChoiceError(view,inp,message){
 clearChoiceError(view,inp);
 const host=inp.closest?.('label')||inp.parentElement;
 const note=document.createElement('p');
 note.className='pg-field-error';
 note.id='pg-choice-error-'+(choiceErrorSeq++);
 note.setAttribute('role','alert');
 note.textContent=message;
 if(host&&host.appendChild)host.appendChild(note);else inp.after?.(note);
 inp.setAttribute('aria-describedby',note.id);
 inp.setAttribute('aria-invalid','true');
}
function clearChoiceError(view,inp){
 inp.removeAttribute('aria-invalid');
 inp.removeAttribute('aria-describedby');
 const host=inp.closest?.('label')||inp.parentElement;
 // 同时清容器里和自己后面的，别把上一处漏下
 (host||inp).querySelectorAll?.('.pg-field-error').forEach(node=>node.remove());
 if(inp.parentElement&&inp.parentElement!==host)inp.parentElement.querySelectorAll?.('.pg-field-error').forEach(node=>node.remove());
}

function drawChoices(view){
 const host=view.querySelector('#pg-choices');
 host.innerHTML='<div class="pg-choices">'+[state.p1,state.p2].map((p,side)=>
  '<fieldset><legend>'+esc(p.name)+'可能会怎么做？</legend>'+p.strategies.map((name,i)=>
   '<label>做法 '+(i+1)+'<input class="form-control form-control-sm pg-choice-name" type="text" data-side="'+(side+1)+'" data-i="'+i+'" value="'+esc(name)+'" aria-label="'+esc(p.name)+'的做法 '+(i+1)+'"></label>'
  ).join('')+'</fieldset>'
 ).join('')+'</div>';
 host.querySelectorAll('.pg-choice-name').forEach(inp=>inp.addEventListener('change',()=>updateChoiceName(view,inp.dataset.side,+inp.dataset.i,inp)));
}

function drawMatrix(view) {
  const host = view.querySelector("#pg-matrix");
  const n1 = state.p1.strategies.length;
  const n2 = state.p2.strategies.length;
  let html = `<div class="matrix-wrap"><table class="matrix">
    <tr><th class="corner"></th><th class="corner" colspan="${n2}">${esc(state.p2.name)} ↓</th></tr>
    <tr><th class="corner">${esc(state.p1.name)} →</th>`;
  for (let j = 0; j < n2; j++) {
    html += `<th><input type="text" aria-label="做法名称" class="form-control form-control-sm strat-name" data-side="2" data-i="${j}" value="${esc(
      state.p2.strategies[j]
    )}" style="width:86px; text-align:center; padding:2px 4px"></th>`;
  }
  for (let i = 0; i < n1; i++) {
    html += `</tr><tr><th><input type="text" aria-label="做法名称" class="form-control form-control-sm strat-name" data-side="1" data-i="${i}" value="${esc(
      state.p1.strategies[i]
    )}" style="width:86px; text-align:center; padding:2px 4px"></th>`;
    for (let j = 0; j < n2; j++) {
      const key = cellKey(i, j);
      const v = state.cells[key] || ["", ""];
      const hit = lastSol && eqCellKeys(lastSol, strategiesOf()).has(key) ? " eq-hit" : "";
      html += `<td class="cell${hit}">
        <input type="number" step="any" class="form-control form-control-sm payoff" data-i="${i}" data-j="${j}" data-k="0" value="${esc(
          v[0]
        )}" aria-label="我的好坏" placeholder="我方">
        <input type="number" step="any" class="form-control form-control-sm payoff" data-i="${i}" data-j="${j}" data-k="1" value="${esc(
          v[1]
        )}" aria-label="对方的好坏" placeholder="对方">
      </td>`;
    }
  }
  html += `</table></div>`;
  host.innerHTML = html;

  host.querySelectorAll(".strat-name").forEach((inp) =>
    inp.addEventListener("change", () => {
      updateChoiceName(view,inp.dataset.side,+inp.dataset.i,inp);
    })
  );
  host.querySelectorAll(".payoff").forEach((inp) =>
    inp.addEventListener("input", () => {
      const { i, j, k } = inp.dataset;
      const key = cellKey(+i, +j);
      const cell = state.cells[key] || ["", ""];
      cell[+k] = inp.value;
      state.cells[key] = cell;
      confirmOrigin(+i, +j, +k);
      drawOutcomes(view);
      drawSpec(view);
    })
  );
}

function keptEquilibria(sol){
 const all=sol?.equilibria||[],survivors=sol?.refinement?.survivors;
 return Array.isArray(survivors)?all.filter(eq=>survivors.includes(eq.id)):all;
}
function pureKeys(sol){
 return new Set(keptEquilibria(sol).filter(eq=>eq.kind==='Pure'&&eq.support?.[0]?.length===1&&eq.support?.[1]?.length===1)
  .map(eq=>`${eq.support[0][0]},${eq.support[1][0]}`));
}

// ── 「这格好坏是谁判断的」：示例继承必须看得见（UI-009）─────────────────────
// 例子的好坏是教学演示，不是用户的判断。用户只改个名字时，旧数值会跟着搬过去，
// 于是「我改成自己的情况了」和「这些好坏是我自己选的」两句话同时成立——后者是假的。
// 因此逐格记录来源，界面标出来、结论里说清楚，用户核对过一格才消一格。

/** 某一格是否仍来自示例（省略 side 时看整格：任一侧未核对就算）。 */
export function exampleOriginIn(current,i,j,side){
 const flag=current.origin?.[`${current.p1.strategies[i]},${current.p2.strategies[j]}`];
 if(!flag)return false;
 return side==null?flag[0]||flag[1]:Boolean(flag[side]);
}
function exampleOrigin(i,j,side){return exampleOriginIn(state,i,j,side);}

/** 用户亲手改过这一格的某一侧 ⇒ 那一侧不再是示例判断。 */
function confirmOrigin(i,j,k){
 const key=cellKey(i,j),flag=state.origin?.[key];
 if(flag&&flag[k]){flag[k]=false;}
}

/** 仍未核对的示例判断： [{i,j,label}]，供提示条与结论引用。 */
export function pendingExamplesIn(current){
 const out=[];
 for(let i=0;i<current.p1.strategies.length;i++)for(let j=0;j<current.p2.strategies.length;j++){
  if(exampleOriginIn(current,i,j))out.push({i,j,label:`${current.p1.strategies[i]}／${current.p2.strategies[j]}`});
 }
 return out;
}
function pendingExamples(){return pendingExamplesIn(state);}

/** 提示条正文：有未核对的示例判断就说明还有哪几格，没有则空串。 */
function examplesNotice(){return examplesNoticeIn(state);}
export function examplesNoticeIn(current){
 const pending=pendingExamplesIn(current);
 if(!pending.length)return '';
 return '<p class="pg-example-warning" role="status">还有 <strong>'+pending.length+'</strong> 项好坏是例子里的，你还没核对：'+esc(pending.map(x=>x.label).join('；'))+'。改成你的情况后请逐个过一遍——没核对的不会算成你自己的判断。</p>';
}

/** 把「还没选」的虚框标记同步到某一格（与 markExample 同理，只动这一格）。*/
function markUnknown(section,i,j){
 if(!section)return;
 const cell=state.cells[cellKey(i,j)]||['',''];
 const unknown=(cell[0]==null||cell[0]==='')||(cell[1]==null||cell[1]==='');
 section.classList.toggle('is-unknown',unknown);
}

/** 把「示例待核」标记同步到某一格（改了一个选择后只动这一格，不整块重绘）。 */
function markExample(section,i,j){
 if(!section)return;
 const on=exampleOrigin(i,j);
 section.classList.toggle('is-example',on);
 const old=section.querySelector('.pg-example-tag');
 if(on&&!old){
  const tag=document.createElement('span');
  tag.className='pg-example-tag';
  tag.title='这一格还是例子里的判断，你还没核对过';
  tag.textContent='示例待核';
  section.prepend(tag);
 }else if(!on&&old)old.remove();
}

/** 刷新提示条：第一次出现、内容变化、清零时各自处理。 */
function refreshExampleNotice(host){
 const html=examplesNotice(),old=host.querySelector('.pg-example-warning');
 if(!html){old?.remove();return;}
 if(old)old.outerHTML=html;else host.insertAdjacentHTML('afterbegin',html);
}

function drawOutcomes(view){
 const host=view.querySelector('#pg-outcomes'),stable=pureKeys(lastSol);
 let html=examplesNotice();
 html+='<div class="pg-outcome-grid">';
 for(let i=0;i<state.p1.strategies.length;i++)for(let j=0;j<state.p2.strategies.length;j++){
  const cell=state.cells[cellKey(i,j)]||['',''];
  const example=exampleOrigin(i,j);
  const select=(value,side)=>{
   const raw=value==null?'':String(value),known=FEELINGS.some(f=>String(f.value)===raw);
   return '<select class="form-select form-select-sm pg-feeling" data-i="'+i+'" data-j="'+j+'" data-k="'+side+'" aria-label="'+esc(state.p1.name)+'选择'+esc(state.p1.strategies[i])+'、'+esc(state.p2.name)+'选择'+esc(state.p2.strategies[j])+'时，'+esc(side===0?state.p1.name:state.p2.name)+'觉得好还是吃亏">'+
    '<option value=""'+(raw===''?' selected':'')+'>还不知道</option>'+
    (!known&&raw!==''?'<option value="'+esc(raw)+'" selected>已有数字 '+esc(raw)+'（在详细设置里改）</option>':'')+
    FEELINGS.map(f=>'<option value="'+f.value+'"'+(raw===String(f.value)?' selected':'')+'>'+f.label+'</option>').join('')+'</select>';
  };
  const unknownSide=cell[0]==null||cell[0]===''?0:(cell[1]==null||cell[1]===''?1:null);
  html+=`<section class="pg-outcome${stable.has(`${i},${j}`)?' is-result':''}${example?' is-example':''}${unknownSide!=null?' is-unknown':''}" id="pg-cell-${i}-${j}">
    ${example?'<span class="pg-example-tag" title="这一格还是例子里的判断，你还没核对过">示例待核</span>':''}
    <h4>${esc(state.p1.name)}：${esc(state.p1.strategies[i])}<br>${esc(state.p2.name)}：${esc(state.p2.strategies[j])}</h4>
    <div class="pg-score-fields"><label>${esc(state.p1.name)}会觉得${select(cell[0],0)}</label>
    <label>${esc(state.p2.name)}会觉得${select(cell[1],1)}</label></div>
  </section>`;
 }
 host.innerHTML=html+'</div>';
 host.querySelectorAll('.pg-feeling').forEach(inp=>inp.addEventListener('change',()=>{
  const {i,j,k}=inp.dataset,key=cellKey(+i,+j),cell=state.cells[key]||['',''];
  cell[+k]=inp.value;state.cells[key]=cell;
  confirmOrigin(+i,+j,+k);
  // 只更新这一格和提示条，不整块重绘——重绘会抢走焦点（键盘用户改完就不知道自己在哪了）。
  const section=inp.closest?.('.pg-outcome');
  if(section){markExample(section,+i,+j);markUnknown(section,+i,+j);}
  refreshExampleNotice(host);
  const paired=view.querySelector(`.payoff[data-i="${i}"][data-j="${j}"][data-k="${k}"]`);
  if(paired)paired.value=inp.value;
  drawSpec(view);
 }));
}

/**
 * 解释求解结果。
 *
 * @param pending 仍是示例判断、用户未核对的格子（pendingExamples() 的结果）。
 *   传空数组表示全部由用户核对过。**省略该参数时按「来源不明」处理**——
 *   拿不出来历就不能声称判断属于用户，这一侧的默认必须是保守的。
 */
export function explainSolution(sol,p1,p2,pending){
 const equilibria=keptEquilibria(sol);
 if(!equilibria.length)return '<p>按现在选的条件，没找到能留下来的做法。详情看下面。</p>';
 const pure=equilibria.filter(eq=>eq.kind==='Pure'&&eq.support?.[0]?.length===1&&eq.support?.[1]?.length===1);
 const mixed=equilibria.length-pure.length;
 const unconfirmed=Array.isArray(pending)?pending:null;
 const allConfirmed=unconfirmed&&unconfirmed.length===0;
 let html='<div class="pg-explanation"><h3>'+(allConfirmed?'只看你核对过的好坏':'按现在表里的好坏')+'，有 '+equilibria.length+' 种做法可以继续比较</h3>';
 if(pure.length)html+='<p>这些情况里，对方不改，你单改也不会更划算：</p><ul>'+pure.map(eq=>{
  const a=p1.strategies[eq.support[0][0]],b=p2.strategies[eq.support[1][0]];
  return '<li>'+esc(p1.name)+'选择<strong>'+esc(a)+'</strong>，'+esc(p2.name)+'选择<strong>'+esc(b)+'</strong></li>';
 }).join('')+'</ul>';
 if(mixed)html+='<p>另外还有 '+mixed+' 种“有时这样、有时那样”的计算结果。它只是另一种试算方式，不是在说现实中各做法会出现多少次。</p>';
 if(equilibria.length>1)html+='<p>可能的做法不止一种。光凭这张试填的表，不能知道现实里会发生哪一种。</p>';
 if(sol.completeness&&sol.completeness!=='Complete')html+='<p>可能还有其他做法没有找全。</p>';
 if(unconfirmed===null){
  // 拿不到来源就不许说「是你自己选的」，也不编一个数目出来。
  html+='<p class="pg-result-caution is-warning">这些好坏的来源没能确定（哪几格仍是示例、哪几格是你核对的，这次没取到）。在弄清楚之前，<strong>不要把这当成你自己条件下的比较</strong>。</p>';
 }else if(unconfirmed.length){
  html+='<p class="pg-result-caution is-warning">还有 '+unconfirmed.length+' 项好坏是例子里的，你没有核对过（'+esc(unconfirmed.map(x=>x.label).join('；'))+'）。程序先按它们比；在你逐个核对之前，<strong>不要把这当成你自己条件下的比较</strong>。</p>';
 }
 html+='<p class="pg-result-caution">'+(allConfirmed?'这些好坏是你自己核对过的':'现在表里的好坏')+'，程序只按它们比较。它不知道对方真正会做什么，也不能替你决定怎么做。</p></div>';
 return html;
}

function cellKey(i, j) {
  return `${state.p1.strategies[i]},${state.p2.strategies[j]}`;
}

export function renameChoiceInState(current,side,idx,newName) {
  const strat = side === "1" ? current.p1.strategies : current.p2.strategies;
  const other = side === "1" ? current.p2.strategies : current.p1.strategies;
  const cells = {}, origin = {};
  for (let i = 0; i < strat.length; i++) {
    for (let j = 0; j < other.length; j++) {
      const from =
        side === "1" ? `${strat[i]},${other[j]}` : `${other[j]},${strat[i]}`;
      const to =
        side === "1"
          ? `${i === idx ? newName : strat[i]},${other[j]}`
          : `${other[j]},${i === idx ? newName : strat[i]}`;
      if (current.cells[from] != null) cells[to] = current.cells[from];
      // 来源必须跟着数值一起搬家：只搬数值不搬来源，等于把示例判断悄悄洗成用户判断
      // （正是 UI-009 的成因）。复制数组而不再是同一引用，免得原地改一格污染另一格。
      if (current.origin?.[from] != null) origin[to] = [...current.origin[from]];
    }
  }
  strat[idx] = newName;
  current.cells = cells;
  current.origin = origin;
}

function strategiesOf() {
  return { [state.p1.name]: state.p1.strategies, [state.p2.name]: state.p2.strategies };
}

// ── GameSpec 构造（与 gamespec crate 的 JSON 契约一致） ─────────────────────

export function buildSpecFromState(s) {
  const cells = {};
  let complete = true;
  for (const sa of s.p1.strategies) {
    for (const sb of s.p2.strategies) {
      const v = s.cells[`${sa},${sb}`];
      const a = v?.[0] !== "" && v?.[0] != null ? Number(v[0]) : null;
      const b = v?.[1] !== "" && v?.[1] != null ? Number(v[1]) : null;
      if (a == null || b == null || !Number.isFinite(a) || !Number.isFinite(b)) {
        complete = false;
        continue;
      }
      cells[`${sa},${sb}`] = {
        [s.p1.name]: { kind: "literal", value: a },
        [s.p2.name]: { kind: "literal", value: b },
      };
    }
  }
  return {
    game_id: "web-playground",
    players: [
      { id: s.p1.name, name: s.p1.name, vn_role: "focal", relation_to_game: "player" },
      { id: s.p2.name, name: s.p2.name, vn_role: "substitutor", relation_to_game: "player" },
    ],
    strategies: { [s.p1.name]: s.p1.strategies, [s.p2.name]: s.p2.strategies },
    payoffs: { form: "normal", values: { cells } },
    params: {},
    information: { type: "complete" },
    dynamics: { form: "one_shot" },
    meta: { origin: "web-console" },
    __complete: complete, // 前端提示用，发送前删除
  };
}
function buildSpec(){return buildSpecFromState(state);}

function drawSpec(view) {
  const spec = buildSpec();
  const { __complete, ...rest } = spec;
  view.querySelector("#pg-spec-pre").textContent = JSON.stringify(rest, null, 2);
  // 草稿的统一落盘点：改名、改好坏、加/减做法、按数字摆局、打开存档都经过这里。
  // 放在这里而不是逐个 handler 里调用，是为了不漏——漏一条就是「以为存了其实没存」。
  persistDraft();
  return spec;
}

// ── 求解 ────────────────────────────────────────────────────────────────────

function showResult(view) {
  const card = view.querySelector('#pg-result-card');
  if (!card?.isConnected) return;
  card.focus({preventScroll:true});
  card.scrollIntoView({block:'nearest',behavior:'auto'});
}

async function doSolve(view) {
  const btn = view.querySelector("#pg-solve");
  const result = view.querySelector("#pg-result");
  const sub = view.querySelector("#pg-result-sub");
  const spec = drawSpec(view);
  const solvingRevision = revision;
  if (!spec.__complete) {
    // 列清楚是哪几种组合、哪一方还不知道，并给回到字段的锚点
    result.innerHTML = incompleteNotice(state) || `<div class="empty"><p class="empty-title text-danger">还有没选的</p><p class="empty-subtitle">每种情况都要选两边觉得怎样，才能比。不清楚的先别猜。</p></div>`;
    sub.textContent = '等补齐';
    result.querySelectorAll('.pg-unknown-list a[href^="#pg-cell-"]').forEach(a=>{
      a.addEventListener('click',(e)=>{
        e.preventDefault();
        const target=a.getAttribute('href').slice(1);
        document.getElementById(target)?.scrollIntoView({block:'center',behavior:'smooth'});
        document.getElementById(target)?.querySelector?.('.pg-feeling')?.focus?.();
      });
    });
    showResult(view);
    return;
  }
  const { __complete, ...payload } = spec;
  btn.disabled = true;
  btn.innerHTML = `<span class="spinner-border spinner-border-sm me-1"></span>正在计算…`;
  sub.textContent = "正在比较…";
  try {
    const body = { spec: payload };
    if (state.method) body.method = state.method;
    if (state.refine) {
      body.refine = state.refine;
      if (state.refine.startsWith("ess")) body.refine_denominator = state.refineDen;
    }
    const res = await api.solve(body);
    if (solvingRevision !== revision) return;
    lastSol = res.result;
    const stratMap = strategiesOf();
    const names = { [state.p1.name]: state.p1.name, [state.p2.name]: state.p2.name };
    result.innerHTML = explainSolution(lastSol,state.p1,state.p2,pendingExamples());
    view.querySelector("#pg-result-technical").innerHTML = equilibriaCards(lastSol, stratMap, names);
    sub.textContent = `找到 ${keptEquilibria(lastSol).length} 种情况`;
    view.querySelector("#pg-result-matrix").innerHTML =
      `<div class="subheader mb-1">在表里的位置（◆）</div>` +
      payoffMatrix(payload, { equilibriumCells: eqCellKeys({...lastSol,equilibria:keptEquilibria(lastSol)}, stratMap) });
    drawMatrix(view); // 编辑矩阵同步高亮
    drawOutcomes(view);
    showResult(view);
  } catch (e) {
    lastSol = null;
    result.innerHTML = `<p class="text-danger">还比不了。看看两边做法和好坏填齐没。</p><details class="raw"><summary>查看具体原因</summary><pre>${esc(e.message)}</pre></details>`;
    sub.textContent = "没算成";
    view.querySelector("#pg-result-technical").innerHTML = "";
    view.querySelector("#pg-result-matrix").innerHTML = "";
    showResult(view);
  } finally {
    btn.disabled = false;
    btn.textContent = "看看结果";
  }
}

// ── 事件绑定 ────────────────────────────────────────────────────────────────

/** 当前状态相对「刚点开某个示例」有没有被用户动过。
 *  用来判断切换示例前该不该问——没动过就没什么可覆盖的。*/
export function stateModified(current, preset) {
  if (!current || !preset) return false;
  const fresh = freshExampleState(preset);
  if (current.p1.name !== fresh.p1.name || current.p2.name !== fresh.p2.name) return true;
  const sameList = (a, b) => a.length === b.length && a.every((x, i) => x === b[i]);
  if (!sameList(current.p1.strategies, fresh.p1.strategies)) return true;
  if (!sameList(current.p2.strategies, fresh.p2.strategies)) return true;
  for (const key of Object.keys(current.cells || {})) {
    if (JSON.stringify(current.cells[key]) !== JSON.stringify(fresh.cells[key])) return true;
  }
  for (const key of Object.keys(fresh.cells || {})) {
    if (JSON.stringify(current.cells?.[key]) !== JSON.stringify(fresh.cells[key])) return true;
  }
  if (current.method !== fresh.method || current.refine !== fresh.refine || current.refineDen !== fresh.refineDen) return true;
  return false;
}

/** 覆盖前要说清楚会丢什么：名称、做法、好坏、计算设置四项各自有没有被改过。*/
export function overwriteScope(current, preset) {
  const fresh = freshExampleState(preset);
  const names = current.p1.name !== fresh.p1.name || current.p2.name !== fresh.p2.name;
  const strategies = current.p1.strategies.join('|') !== fresh.p1.strategies.join('|')
    || current.p2.strategies.join('|') !== fresh.p2.strategies.join('|');
  const scores = Object.keys(current.cells || {}).some(
    (key) => JSON.stringify(current.cells[key]) !== JSON.stringify(fresh.cells[key]))
    || Object.keys(fresh.cells || {}).some((key) => current.cells?.[key] == null);
  const settings = current.method !== fresh.method || current.refine !== fresh.refine || current.refineDen !== fresh.refineDen;
  return [names ? '双方名称' : '', strategies ? '做法' : '', scores ? '好坏选择' : '', settings ? '计算设置' : ''].filter(Boolean);
}


/** 切换示例前的确认：说清会覆盖什么，并给「保留当前内容」的退路。*/
function askBeforePreset(view, key){
 const preset=PRESETS[key];
 const scope=overwriteScope(state,preset);
 const host=view.querySelector('#pg-preset-confirm');
 if(!host)return;
 host.hidden=false;
 host.innerHTML='<p class="pg-preset-confirm-text">换成「'+esc(preset.label)+'」会覆盖你改过的：'+
  esc(scope.join('、'))+'。已填的好坏不会自动搬过去。</p>'+
  '<div class="choice-list"><button class="choice-button" id="pg-preset-ok">换成这个例子</button>'+
  '<button class="choice-button" id="pg-preset-cancel">保留我现在的</button></div>';
 host.querySelector('#pg-preset-ok').onclick=()=>{
  host.hidden=true;host.innerHTML='';
  initState(preset);currentPresetKey=key;
  applyPresetToView(view);
 };
 host.querySelector('#pg-preset-cancel').onclick=()=>{
  host.hidden=true;host.innerHTML='';
  view.querySelector('#pg-result').innerHTML='<p class="small-note">没换例子，你改的还在。</p>';
  view.querySelector('#pg-result-sub').textContent='条件没变';
 };
}

/** 换示例后把界面各区块同步一遍（原 bindPresets 里那段抽出来复用）。*/
function applyPresetToView(view){
 view.querySelector('#pg-p1-name').value=state.p1.name;
 view.querySelector('#pg-p2-name').value=state.p2.name;
 view.querySelector('#pg-method').value=state.method;
 view.querySelector('#pg-refine').value=state.refine;
 drawMatrix(view);drawChoices(view);drawOutcomes(view);drawSpec(view);
 view.querySelector('#pg-result').innerHTML='<div class="empty"><p class="empty-title">例子已换好</p><p class="empty-subtitle">可以改做法和好坏，再点“看看结果”。</p></div>';
 view.querySelector('#pg-result-sub').textContent='还没开始';
 view.querySelector('#pg-result-technical').innerHTML='';
 view.querySelector('#pg-result-matrix').innerHTML='';
}

function bindPresets(view) {
  view.querySelectorAll("[data-preset]").forEach((btn) =>
    btn.addEventListener("click", () => {
      const preset = PRESETS[btn.dataset.preset];
      // 判「有没有改过」要跟**当前这个示例**比，不是跟要换成的那个比——
      // 没动过就没什么可覆盖的，问也是白问。点当前示例同样走这条：
      // 改过就问，没改过就是空操作（原来会静默清空修改）。
      if (stateModified(state, PRESETS[currentPresetKey])) {
        askBeforePreset(view, btn.dataset.preset);
        return;
      }
      initState(preset);
      currentPresetKey = btn.dataset.preset;
      applyPresetToView(view);
    })
  );
}

function bindControls(view) {
  view.querySelector("#pg-p1-name").value = state.p1.name;
  view.querySelector("#pg-p2-name").value = state.p2.name;
  view.querySelector("#pg-p1-name").addEventListener("change", (e) => {
    const old = state.p1.name;
    const name=e.target.value.trim()||old;
    if(name===state.p2.name){e.target.value=old;view.querySelector('#pg-result').innerHTML='<p class="text-danger">双方名称不能相同。原来的名称已保留。</p>';return;}
    state.p1.name = name;
    e.target.value = state.p1.name;
    drawMatrix(view);
    drawChoices(view);
    drawOutcomes(view);
    drawSpec(view);
  });
  view.querySelector("#pg-p2-name").addEventListener("change", (e) => {
    const old = state.p2.name;
    const name=e.target.value.trim()||old;
    if(name===state.p1.name){e.target.value=old;view.querySelector('#pg-result').innerHTML='<p class="text-danger">双方名称不能相同。原来的名称已保留。</p>';return;}
    state.p2.name = name;
    e.target.value = state.p2.name;
    drawMatrix(view);
    drawChoices(view);
    drawOutcomes(view);
    drawSpec(view);
  });
  view.querySelector("#pg-method").addEventListener("change", (e) => (state.method = e.target.value));
  view.querySelector("#pg-refine").addEventListener("change", (e) => {
    state.refine = e.target.value;
    view.querySelector("#pg-den-field").style.display = state.refine.startsWith("ess") ? "" : "none";
  });
  view.querySelector("#pg-den").addEventListener("change", (e) => {
    state.refineDen = Math.min(64, Math.max(2, Number(e.target.value) || 6));
    e.target.value = state.refineDen;
  });
  view.querySelector("#pg-solve").addEventListener("click", () => doSolve(view));

  view.querySelector("#pg-add-strat").addEventListener("click", () => {
    if (state.p1.strategies.length >= 4 || state.p2.strategies.length >= 4) return;
    invalidate(view);
    addStrategy("1");
    addStrategy("2");
    drawMatrix(view);
    drawChoices(view);
    drawOutcomes(view);
    drawSpec(view);
  });
  view.querySelector("#pg-del-strat").addEventListener("click", () => {
    if (state.p1.strategies.length <= 2) return;
    invalidate(view);
    dropLastStrategy("1");
    dropLastStrategy("2");
    drawMatrix(view);
    drawChoices(view);
    drawOutcomes(view);
    drawSpec(view);
  });
}

function bindNumbers(view) {
  view.querySelector("#pg-poi-run").addEventListener("click", () => runPoi(view));
  view.querySelector("#pg-numbers-run").addEventListener("click", async () => {
    const button = view.querySelector("#pg-numbers-run");
    const result = view.querySelector("#pg-result");
    const names = { me: state.p1.name, rival: state.p2.name };
    button.disabled = true;
    button.textContent = "正在按数字摆局…";
    try {
      const compiled = await api.model({ bundle: numbersBundle(view, names) });
      if (!compiled.ok) {
        result.innerHTML = numbersReport(compiled, names);
        view.querySelector("#pg-result-sub").textContent = "数字没填齐";
        return;
      }
      // 编译结果直接写进 state：下面的矩阵、选择框、求解全部沿用既有路径。
      const strategies = compiled.spec.strategies;
      state.p1.strategies = [...strategies.me];
      state.p2.strategies = [...strategies.rival];
      state.cells = {};
      // 这些数字来自用户自己在面板里填的事实与算式，不是示例判断。
      state.origin = {};
      for (const c of compiled.cells) state.cells[c.me_move + "," + c.rival_move] = [c.me_profit, c.rival_profit];
      state.method = "support_enumeration";
      state.refine = "";
      lastSol = null;
      revision++;
      drawChoices(view); drawOutcomes(view); drawMatrix(view); drawSpec(view);
      lastRobustness = null;
      result.innerHTML = numbersReport(compiled, names) + robustnessPanel() + savePanel();
      view.querySelector("#pg-robust").addEventListener("click", () => runRobustness(view, names));
      view.querySelector("#pg-save").addEventListener("click", () => saveModel(view, names));
      view.querySelector("#pg-result-sub").textContent = "矩阵已按数字算好";
    } catch (error) {
      result.innerHTML = '<p class="text-danger">按数字摆局没成功：' + esc(error.message) + "</p>";
      view.querySelector("#pg-result-sub").textContent = "摆局未完成";
    } finally {
      button.disabled = false;
      button.textContent = "按数字摆局";
    }
  });
}

function addStrategy(side) {
  const strat = side === "1" ? state.p1.strategies : state.p2.strategies;
  const other = side === "1" ? state.p2.strategies : state.p1.strategies;
  const nm = `做法${strat.length + 1}`;
  for (const o of other) {
    state.cells[side === "1" ? `${nm},${o}` : `${o},${nm}`] = ["", ""];
  }
  strat.push(nm);
}

function dropLastStrategy(side) {
  const strat = side === "1" ? state.p1.strategies : state.p2.strategies;
  const other = side === "1" ? state.p2.strategies : state.p1.strategies;
  const last = strat[strat.length - 1];
  for (const o of other) {
    delete state.cells[side === "1" ? `${last},${o}` : `${o},${last}`];
  }
  strat.pop();
}

function invalidate(view) {
  revision++;
  if (!lastSol && !view.querySelector("#pg-solve")?.disabled) return;
  lastSol = null;
  view.querySelectorAll(".eq-hit").forEach(e => e.classList.remove("eq-hit"));
  view.querySelectorAll(".pg-outcome.is-result").forEach(e => e.classList.remove("is-result"));
  view.querySelector("#pg-result").innerHTML = '<p class="small-note">改了条件，再算一次。</p>';
  view.querySelector("#pg-result-sub").textContent = "条件变了";
  view.querySelector("#pg-result-technical").innerHTML = "";
  view.querySelector("#pg-result-matrix").innerHTML = "";
}