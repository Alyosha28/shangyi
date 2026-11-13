// @ts-check
// 决策帧渲染：把「机器现在怎么理解这个问题」摆成可核对的槽位。
//
// 三态纪律（与 frame_projection.py 一致）：已核实（原文有据）/ 靠假设（标来源）/
// 还缺（未取得）。**空槽不补默认值**，宁可写「还没有」也不猜。
import {esc} from './util.js';
import {FIELD_LABEL} from './research.js';

const SLOT_LIMIT = 4;

function label(key) {
  return FIELD_LABEL[key] || key;
}

function list(items, render, empty) {
  if (!items?.length) return '<span class="frame-slot-empty">' + empty + '</span>';
  const shown = items.slice(0, SLOT_LIMIT).map(render).join('、');
  return shown + (items.length > SLOT_LIMIT ? ' 等 ' + items.length + ' 项' : '');
}

/** 帧 → HTML。frame 缺失或有空槽时如实说明，不假装知道。 */
export function renderFrame(frame) {
  if (!frame) return '';
  const tags = [frame.type, frame.mode, frame.scope].filter(Boolean)
    .map((t) => '<span class="frame-tag">' + esc(t) + '</span>').join('');

  // 空槽的措辞要分清两种事实：这次没指定动作（看可能），还是存档没单独留动作记录（试做法）。
  const move = frame.move?.value
    ? esc(frame.move.value) + (frame.move.via ? '<em>' + esc(frame.move.via) + '</em>' : '')
    : '<span class="frame-slot-empty">' +
      (frame.mode === '试做法' ? '存档里没单独记录，见上面问题' : '还没定（分析里给）') + '</span>';

  const players = list(frame.players, (p) => {
    const moves = p.moves?.length ? '（' + p.moves.map(esc).join(' / ') + '）' : '';
    return esc(p.name) + moves;
  }, '还没摆局');

  const facts = list(frame.facts, (f) => esc(label(f.label)) + ' ' + esc(f.value), '还没有有出处的数字');
  const assumptions = list(frame.assumptions, (a) =>
    esc(label(a.label)) + ' ' + esc(a.value) + (a.via ? '<em>' + esc(a.via) + '</em>' : ''), '暂时没有');
  // 缺口可点：点一下把「挂的租金：」填进输入框，让用户补一个数，而不是面对一张空表。
  const gaps = list(frame.gaps, (g) =>
    '<button type="button" class="frame-gap" data-fill="' + esc(label(g.label)) + '：">' +
    esc(label(g.label)) + '</button>' + (g.why ? '<em>' + esc(g.why) + '</em>' : ''), '暂时没有');

  const result = frame.result
    ? esc(frame.result.equilibria + ' 种稳的做法') +
      (frame.result.completeness ? '<em>' + esc(frame.result.completeness) + '</em>' : '') +
      (frame.result.has_recommendation ? '<em>已有建议</em>' : '')
    : '<span class="frame-slot-empty">还没算</span>';

  return '<section class="frame-card">' +
    '<div class="evidence-group">机器这样理解你的问题</div>' +
    (frame.question ? '<p class="frame-question">' + esc(frame.question) + '</p>' : '') +
    (tags ? '<div class="frame-tags">' + tags + '</div>' : '') +
    '<dl class="frame-slots">' +
      '<div><dt>我方能做</dt><dd>' + move + '</dd></div>' +
      '<div><dt>局里有谁</dt><dd>' + players + '</dd></div>' +
      '<div><dt>已核实</dt><dd>' + facts + '</dd></div>' +
      '<div><dt>靠假设</dt><dd>' + assumptions + '</dd></div>' +
      '<div><dt>还缺</dt><dd>' + gaps + '</dd></div>' +
      '<div><dt>结果</dt><dd>' + result + '</dd></div>' +
    '</dl>' +
    '<p class="frame-note">这张表就是机器现在对你的理解。不对的地方，直接在对话里说。</p>' +
    '</section>';
}

/** 周边事实（高德 POI）：计数与来源；没取得的类目如实列出，不写 0。 */
export function renderPoi(surveys) {
  const list = (surveys || []).filter((s) => s && (s.counts?.length || s.brand || s.keywords || s.unavailable?.length));
  if (!list.length) return '';
  return list.map((s) => {
    const row = (c) => '<li>' + esc(c.label) + '：' + esc(String(c.count)) + ' 家</li>';
    const rows = (s.counts || []).map(row).join('') +
      (s.brand ? row(s.brand) : '') + (s.keywords ? row(s.keywords) : '');
    const missing = (s.unavailable || []).length
      ? '<details class="pg-flags"><summary>这次没取得的（' + s.unavailable.length + '）</summary><ul>' +
        s.unavailable.map((u) => '<li>' + esc(u) + '</li>').join('') + '</ul></details>'
      : '';
    return '<div class="evidence-group">周边事实（高德 POI）</div>' +
      '<div class="frame-card"><p class="frame-question">' + esc(s.address || '') +
      ' · 半径 ' + esc(String(s.radius || '')) + ' 米</p>' +
      '<ul class="poi-counts">' + rows + '</ul>' + missing +
      '<p class="frame-note">来源：' + esc(s.source || '高德') + '。计数是门店数量，不是营业额或客流。</p></div>';
  }).join('');
}
