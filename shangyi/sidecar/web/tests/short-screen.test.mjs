import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import path from 'node:path';

// UI-001 的回归护栏（research/qa/2026-10-04-frontend-issues）。
//
// 这个缺陷**只存在于 CSS 里**：390×420 视口展开「分析设置」后，
// 输入区高约 448px，而 `.app-shell{overflow:hidden}`、只有 `#view` 可滚、
// `#composer-host{flex:none}` 不许收缩 ⇒「开始咨询」落在 451–485px 被裁，
// 整页没有第二处可滚区域，用户无法提交这类咨询。
//
// JS 单测抓不到它（布局在样式表里），所以这里**直接对样式表立契约**：
// 矮屏下输入区必须同时做到「可收缩 / 有上限 / 自身可滚」，缺一件就回到裁切。
// 改动 styles.css 前先想清楚这三件为什么都在，别为「顺手清理」删掉其中一件。

const css = readFileSync(
  path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'styles.css'),
  'utf8',
);

/** 取 `@media(<feature>)` 块的正文（本项目没有嵌套 media，配平括号即可）。 */
function mediaBlock(feature) {
  const start = css.indexOf('@media(' + feature + ')');
  assert.notEqual(start, -1, `缺少 @media(${feature}) 规则`);
  let depth = 0;
  for (let i = css.indexOf('{', start); i < css.length; i++) {
    if (css[i] === '{') depth++;
    else if (css[i] === '}') {
      depth--;
      if (depth === 0) return css.slice(start, i);
    }
  }
  assert.fail(`@media(${feature}) 的花括号没有配平`);
}

/** 取某个选择器在给定片段里的声明块正文。 */
function rule(fragment, selector) {
  const at = fragment.indexOf(selector + '{');
  assert.notEqual(at, -1, `在给定片段里找不到 ${selector}{...}`);
  const end = fragment.indexOf('}', at);
  return fragment.slice(at + selector.length + 1, end);
}

/** 从 `@media(max-height:640px)` 里取出阈值数字。 */
function shortViewportThreshold() {
  const m = css.match(/@media\(max-height:(\d+)px\)/);
  assert.ok(m, '缺少矮屏断点 @media(max-height:...)');
  return Number(m[1]);
}

test('外壳仍固定一屏并隐藏溢出（这是矮屏问题的成因，不能顺手改掉）', () => {
  const shell = css.slice(css.indexOf('.app-shell{'), css.indexOf('.app-shell{') + 160);
  assert.match(shell, /height:\s*100dvh/, '外壳应固定为一屏（dvh 才会随软键盘收缩）');
  assert.match(shell, /overflow:\s*hidden/, '外壳必须继续裁剪溢出，否则出现双层滚动条');
});

test('正文区仍是可滚的那一个', () => {
  assert.match(rule(css, '#view'), /overflow:\s*auto/);
});

test('矮屏断点覆盖 390×420，且不把所有屏幕都算成矮屏', () => {
  const px = shortViewportThreshold();
  assert.ok(px >= 420, `断点 ${px}px 必须覆盖 QA 实测的 420px 视口`);
  assert.ok(px <= 800, `断点 ${px}px 太宽，等于对所有桌面窗口改写布局`);
});

test('矮屏下输入区三件事同时成立：可收缩、有上限、自身可滚', () => {
  const short = mediaBlock('max-height:' + shortViewportThreshold() + 'px');
  const host = rule(short, '#composer-host');
  assert.match(host, /flex:\s*0 1 auto/, '必须允许收缩：flex:none 正是裁切的成因');
  assert.match(host, /min-height:\s*0/, 'flex 子项默认 min-height:auto，不允许小于内容高');
  assert.match(host, /max-height:\s*\d+dvh/, '必须有高度上限，用 dvh 才不受父级高度是否确定影响');
  assert.match(host, /overflow-y:\s*auto/, '必须有自身滚动，否则上限只是把按钮推出可见区');
  assert.match(host, /overscroll-behavior:\s*contain/, '滚动到边界时不应把页面一起带走');
});

test('矮屏下正文区有最小高度，不被收缩的输入区压没', () => {
  const short = mediaBlock('max-height:' + shortViewportThreshold() + 'px');
  const view = rule(short, '#view');
  const m = view.match(/min-height:\s*(\d+)px/);
  assert.ok(m, `#view 在矮屏下必须有最小高度，实际为：${view}`);
  assert.ok(Number(m[1]) >= 60, `最小高度 ${m[1]}px 太小，问题与结论会没地方显示`);
});

test('矮屏下设置的收起开关常驻可见（粘顶），滚动到底也能收起', () => {
  const short = mediaBlock('max-height:' + shortViewportThreshold() + 'px');
  assert.match(short, /\.conversation-settings summary\{[^}]*position:\s*sticky/);
  assert.match(short, /\.conversation-settings summary\{[^}]*top:\s*0/);
  // 粘顶必须有不透明背景，否则下方内容会透出来
  assert.match(short, /\.conversation-settings summary\{[^}]*background:#fff/);
});
