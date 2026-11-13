#!/usr/bin/env node
// 商奕六枚印记图标 · 嵌入式图标包构建脚本
//
//   node shangyi/ui/shangyi-icons/scripts/build-embedded.mjs
//
// 做什么：把 kubai-icons 的 src/engine.js 与 src/icons.js 里属于商奕的 6 枚图标
// （弈 / 算 / 判 / 证 / 知 / 档）摘出来，包成一个经典脚本片段 dist/embedded.js：
// 无 import/export、零外部依赖、所有内部名字关在 IIFE 里，只暴露 window.ShangYiIcons。
// 契约见 shangyi/ui/WORKBENCH-CONTRACT.md §4 与
// dsh-plugin-shangyi-workbench/src/20-icon.js（唯一调用点：ShangYiIcons.mount(el, name, {accent})）。
//
// 精简：icons.js 里有 40+ 枚通用图标，商奕只用 6 枚，其余全部丢弃；图标依赖的前置常量
// （OL / ACC / DEEP / DEEPS 等）按标识符闭包自动收集，不照抄整段前置代码。
//
// 引擎的唯一改动：engine.js 的 mountIcon 用字符串 'i' 加下标拼 defs id 前缀，
// 同一页面挂两个『弈』会撞 linearGradient / filter / mask 的 id。这里把前缀参数化，
// 由调用方传入单调递增的 uid（'sy1'、'sy2'……），其余引擎代码原样复用。
//
// 纯 Node ESM，只用 node: 内置模块；产物不依赖任何运行时库。

import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const ENGINE_FILE = path.join(ROOT, 'src', 'engine.js');
const ICONS_FILE = path.join(ROOT, 'src', 'icons.js');
const OUT_FILE = path.join(ROOT, 'dist', 'embedded.js');

/** 商奕六枚图标的名字，顺序即色板槽位（见 WORKBENCH-CONTRACT.md §4）。 */
const NAMES = ['弈', '算', '判', '证', '知', '档'];

const fail = (msg) => {
  console.error('build-embedded: ' + msg);
  process.exit(1);
};
const read = (file) => fs.readFileSync(file, 'utf8');
const countOf = (text, needle) => text.split(needle).length - 1;

// ── 1. 版权头：engine.js / icons.js 开头的连续 // 注释 ─────────────────────
function splitHeader(label, source) {
  const lines = source.split('\n');
  let n = 0;
  while (n < lines.length && lines[n].startsWith('//')) n += 1;
  if (n === 0) fail(label + ' 缺少 Kubai Icons 版权注释头');
  return { header: lines.slice(0, n), body: lines.slice(n).join('\n') };
}

// ── 2. 引擎：原样复用，只把 defs id 前缀参数化（唯一注入点）────────────────
const ENGINE_HOOK = "function mountIcon(icon, i, host) {\n  const k = 'i' + i;";
const ENGINE_PATCHED = 'function mountIcon(icon, i, host, uid) {\n'
  + '  // 商奕嵌入版：defs id 前缀由调用方按实例单调递增分配（uid），同页多实例不会撞 id。\n'
  + "  const k = uid || 'i' + i;";

function patchEngine(body) {
  const hits = countOf(body, ENGINE_HOOK);
  if (hits !== 1) {
    fail('src/engine.js 的 mountIcon 结构与预期不符：期望正好 1 处 uid 注入点，实际 ' + hits + ' 处。'
      + '如果引擎升级了，请同步更新 build-embedded.mjs 的 ENGINE_HOOK。');
  }
  return body.replace(ENGINE_HOOK, ENGINE_PATCHED);
}

// ── 3. 图标：按 2 空格缩进把 ICONS 数组切成成员，只留商奕那 6 枚 ────────────
const MEMBER_OPEN = '  {';
const MEMBER_CLOSE = ['  },', '  }'];

function sliceIconObjects(source) {
  const start = source.indexOf('const ICONS = [');
  if (start < 0) fail('src/icons.js 里找不到 `const ICONS = [');
  const end = source.indexOf('\n];', start);
  if (end < 0) fail('src/icons.js 里找不到 ICONS 的结束 `];`');

  const members = [];
  let pending = [];
  let buf = null;
  for (const line of source.slice(start, end + 1).split('\n')) {
    if (buf === null) {
      if (line === MEMBER_OPEN) {
        buf = pending.concat([line]);
        pending = [];
        continue;
      }
      if (line.startsWith('const ICONS')) continue;
      if (line.trim() === '' || line.trim().startsWith('//')) {
        pending.push(line);
        continue;
      }
      fail('src/icons.js 的 ICONS 里有无法按 2 空格缩进切分的行：' + JSON.stringify(line));
    } else {
      buf.push(line);
      if (MEMBER_CLOSE.includes(line)) {
        members.push(buf.join('\n'));
        buf = null;
      }
    }
  }
  if (buf !== null) fail('src/icons.js 的 ICONS 最后一个成员没有闭合');
  return members;
}

function memberName(member) {
  const m = member.match(/^\s{2,}name:\s*'([^']+)',?\s*$/m);
  if (!m) fail('图标成员里找不到 name 字段：' + member.split('\n')[1]);
  return m[1];
}

/** 每个切出来的成员必须能独立通过语法检查，否则说明缩进切分切坏了。 */
function compileMember(member, name) {
  const expr = '(' + member.replace(/,\s*$/, '') + ')';
  try {
    new Function('return ' + expr); // 只编译，不求值（自由变量在调用时才解析）
  } catch (e) {
    fail('图标「' + name + '」的源码切片无法通过语法检查：' + e.message);
  }
}

// ── 4. 图标依赖的前置常量：单行声明建表，按标识符闭包收集 ─────────────────
function readPreamble(source) {
  const end = source.indexOf('const ICONS = [');
  if (end < 0) fail('src/icons.js 里找不到 `const ICONS = [');
  const single = new Map(); // 名字 -> { line, text }，可以直接整行搬走
  const blocked = new Map(); // 名字 -> 起始行，多行声明（本脚本不搬）
  source.slice(0, end).split('\n').forEach((text, lineNo) => {
    const decl = text.match(/^(?:const|let|var|function)\s+([A-Za-z_$][\w$]*)/);
    if (!decl) return;
    const name = decl[1];
    if (/^(?:const|let|var)\s+[A-Za-z_$][\w$]*\s*=.*;\s*$/.test(text)) single.set(name, { line: lineNo, text: text.trim() });
    else blocked.set(name, lineNo);
  });
  return { single, blocked };
}

function collectDependencies(seeds, preamble) {
  const picked = new Map();
  let frontier = seeds.slice();
  for (;;) {
    const next = [];
    for (const text of frontier) {
      for (const m of text.matchAll(/[A-Za-z_$][\w$]*/g)) {
        const name = m[0];
        if (picked.has(name)) continue;
        if (preamble.blocked.has(name)) {
          fail('图标依赖的声明是多行的，本脚本暂不支持自动搬运：' + name
            + '（src/icons.js 第 ' + (preamble.blocked.get(name) + 1) + ' 行）。请扩展 build-embedded.mjs。');
        }
        const hit = preamble.single.get(name);
        if (!hit) continue; // 引擎里的名字 / 局部变量 / 浏览器全局，都不归这里管
        picked.set(name, hit);
        next.push(hit.text);
      }
    }
    if (next.length === 0) break;
    frontier = next;
  }
  return [...picked.entries()].sort((a, b) => a[1].line - b[1].line).map(([, hit]) => hit.text);
}

// ── 5. 运行时：挂载 / 悬停形变 / 主色 ─────────────────────────────────────
// 注意：这段是模板字符串，里面不使用反引号和 ${，避免转义。
const RUNTIME = `
// ---------- 商奕嵌入式运行时 ----------
// 默认主色：宣纸·墨玉 '#1F6B5C'。色板一律由 clearTone 推导（6 个槽位，deepMarks 为真），
// 单实例可用 opts.accent 覆盖，setAccent(hex) 改默认主色并刷新没单独指定 accent 的存活实例。
var DEFAULT_ACCENT = '#1F6B5C';
var TONE_NAME = '墨玉';
var NAMES = ['弈', '算', '判', '证', '知', '档'];
var nextUid = 0;   // 单调递增：每个 mount 实例一个，绝不复用，保证 defs id 唯一
var live = [];     // 存活实例列表，setAccent 时逐个刷新
var defaultTone = clearTone(TONE_NAME, DEFAULT_ACCENT, 6);

// 名字 -> { icon, slot }：slot 是色板槽位（= NAMES 下标），同一图标永远取同一槽位。
var ICON_BY_NAME = (function () {
  var map = {};
  for (var slot = 0; slot < NAMES.length; slot++) {
    for (var j = 0; j < ICONS.length; j++) {
      if (ICONS[j].name === NAMES[slot]) map[NAMES[slot]] = { icon: ICONS[j], slot: slot };
    }
  }
  return map;
})();

function raise(where, msg) {
  throw new Error('ShangYiIcons.' + where + '：' + msg);
}

function normalizeAccent(value, where) {
  if (typeof value !== 'string') {
    raise(where, '主色需要 #RRGGBB 字符串，收到 ' + Object.prototype.toString.call(value));
  }
  var raw = value.trim().replace(/^#/, '');
  if (/^[0-9a-f]{3}$/i.test(raw)) raw = raw[0] + raw[0] + raw[1] + raw[1] + raw[2] + raw[2];
  if (!/^[0-9a-f]{6}$/i.test(raw)) raise(where, '颜色格式应为 #RRGGBB，收到 "' + value + '"');
  return '#' + raw.toUpperCase();
}

// 引擎的 targetOf / render 从 runtime.palette 取色：进作用域切到实例色板，出来还原。
// 所有色板都出自 clearTone（deepMarks 恒为 true），所以动画帧里那次 render 不受切换影响。
function withTone(tone, fn) {
  var prev = runtime.palette;
  runtime.palette = tone;
  try {
    return fn();
  } finally {
    runtime.palette = prev;
  }
}

function toneFor(accent) {
  return clearTone(TONE_NAME, accent, 6);
}

// 按当前 hover 状态重算这一帧要的数值并重画（换主色后用）。
function repaint(inst) {
  return withTone(inst.palette, function () {
    if (inst.raf) {
      cancelAnimationFrame(inst.raf);
      inst.raf = 0;
    }
    inst.cur = targetOf(inst);
    render(inst);
  });
}

function mount(el, name, opts) {
  if (!el || typeof el.querySelector !== 'function' || typeof el.addEventListener !== 'function') {
    raise('mount', '第一个参数必须是可挂载的 DOM 元素');
  }
  var entry = Object.prototype.hasOwnProperty.call(ICON_BY_NAME, name) ? ICON_BY_NAME[name] : null;
  if (!entry) raise('mount', '未知图标 "' + String(name) + '"（可用：' + NAMES.join('、') + '）');

  // 幂等：重复挂载先清掉上一轮的监听与动画帧，再重画。el 里永远只有一枚 svg。
  if (typeof el.__syIconCleanup === 'function') el.__syIconCleanup();

  var accent = opts && opts.accent !== undefined && opts.accent !== null
    ? normalizeAccent(opts.accent, 'mount(opts.accent)')
    : null;
  var tone = accent ? toneFor(accent) : defaultTone;
  var uid = 'sy' + (++nextUid); // 每个实例一个：linearGradient / filter / mask 的 id 都带它
  var inst = withTone(tone, function () {
    return mountIcon(entry.icon, entry.slot, el, uid);
  });
  inst.uid = uid;
  inst.name = name;
  inst.el = el;
  inst.slot = entry.slot;
  inst.palette = tone;
  inst.accent = accent; // null 表示跟随默认主色

  function hover(next) {
    inst.hover = next ? 1 : 0;
    withTone(inst.palette, function () {
      go(inst); // 460ms，cubic-bezier(.22,1,.36,1)
    });
  }
  function onEnter() {
    hover(true);
  }
  function onLeave() {
    hover(false);
  }

  var destroyed = false;
  function destroy() {
    if (destroyed) return;
    destroyed = true;
    el.removeEventListener('mouseenter', onEnter);
    el.removeEventListener('mouseleave', onLeave);
    if (inst.raf) {
      cancelAnimationFrame(inst.raf);
      inst.raf = 0;
    }
    var at = live.indexOf(inst);
    if (at >= 0) live.splice(at, 1);
    if (el.__syIconCleanup === cleanup) {
      el.__syIconCleanup = null;
      el.textContent = '';
    }
  }
  function cleanup() {
    destroy();
  }

  inst.destroy = destroy;
  inst.cleanup = cleanup;
  el.addEventListener('mouseenter', onEnter);
  el.addEventListener('mouseleave', onLeave);
  el.__syIconCleanup = cleanup;
  live.push(inst);
  return inst;
}

function setAccent(value) {
  var accent = normalizeAccent(value, 'setAccent');
  defaultTone = toneFor(accent);
  for (var i = 0; i < live.length; i++) {
    var inst = live[i];
    if (inst.accent) continue; // 单独指定过 accent 的实例保持自己的颜色
    inst.palette = defaultTone;
    repaint(inst);
  }
  return accent;
}
`;

// ── 6. 拼产物 ────────────────────────────────────────────────────────────
function build() {
  const engine = splitHeader('src/engine.js', read(ENGINE_FILE));
  const icons = splitHeader('src/icons.js', read(ICONS_FILE));
  const engineBody = patchEngine(engine.body);

  // 6 枚商奕图标：其余旧图标丢弃。
  const members = sliceIconObjects(icons.body);
  const picked = new Map();
  for (const member of members) {
    const name = memberName(member);
    if (!NAMES.includes(name)) continue;
    if (picked.has(name)) fail('src/icons.js 里出现重复图标「' + name + '」');
    compileMember(member, name);
    picked.set(name, member);
  }
  const missing = NAMES.filter((name) => !picked.has(name));
  if (missing.length) fail('src/icons.js 里缺少商奕图标：' + missing.join('、'));

  // 图标依赖的前置常量（OL / ACC / DEEP / DEEPS ……），按源码顺序搬。
  const consts = collectDependencies(NAMES.map((name) => picked.get(name)), readPreamble(icons.body));

  const lines = [];
  lines.push('// AUTO-GENERATED by scripts/build-embedded.mjs — 改 src/engine.js 或 src/icons.js 后重跑，不要手改本文件。');
  lines.push('// 商奕六枚印记图标（弈 / 算 / 判 / 证 / 知 / 档）嵌入式图标包：经典脚本片段，无 import/export，');
  lines.push('// 零外部依赖，可整段拼进 DSH 客户端 bundle。契约见 shangyi/ui/WORKBENCH-CONTRACT.md §4。');
  lines.push('//');
  engine.header.forEach((line) => lines.push(line));
  lines.push('// 图标几何、配色、动效与运行时复用 kubai-icons（src/engine.js / src/icons.js），版权头如上。');
  lines.push('// 引擎唯一改动：mountIcon 的 defs id 前缀改由调用方传入的实例 uid（同页多实例不撞 id）。');
  lines.push('//');
  lines.push('// 用法：');
  lines.push("//   var inst = ShangYiIcons.mount(el, '弈');                        // 默认主色 #1F6B5C（宣纸·墨玉）");
  lines.push("//   var inst = ShangYiIcons.mount(el, '算', { accent: '#8A2F2B' }); // 单实例主色，不影响别的实例");
  lines.push("//   ShangYiIcons.setAccent('#8A2F2B');                             // 改默认主色，刷新未单独指定 accent 的实例");
  lines.push('//   inst.destroy(); 或 el.__syIconCleanup();                        // 摘监听、停动画、清空 svg');
  lines.push('//   枚举：ShangYiIcons.names');
  lines.push('');
  lines.push('var ShangYiIcons = (function () {');
  lines.push("  'use strict';");
  lines.push('');
  lines.push('// ===== 片段 1/3：kubai-icons 引擎（src/engine.js，原样复用，仅 uid 注入点不同）=====');
  lines.push(engineBody.replace(/^\n+/, '').trimEnd());
  lines.push('');
  lines.push('// ===== 片段 2/3：6 枚商奕图标（src/icons.js）及它们依赖的前置常量 =====');
  consts.forEach((line) => lines.push(line));
  lines.push('');
  lines.push('var ICONS = [');
  lines.push(NAMES.map((name) => picked.get(name).replace(/,\s*$/, '')).join(',\n'));
  lines.push('];');
  lines.push('');
  lines.push('// ===== 片段 3/3：商奕嵌入式运行时（挂载 / 悬停形变 / 主色）=====');
  lines.push(RUNTIME.replace(/^\n+/, '').trimEnd());
  lines.push('');
  lines.push('return {');
  lines.push("  names: NAMES.slice(),   // ['弈', '算', '判', '证', '知', '档']");
  lines.push('  mount: mount,');
  lines.push('  setAccent: setAccent');
  lines.push('};');
  lines.push('})();');
  lines.push('');
  lines.push("if (typeof window !== 'undefined' && window.ShangYiIcons === undefined) { window.ShangYiIcons = ShangYiIcons; }");
  lines.push('');

  const text = lines.join('\n');

  // ── 产物自检：语法、经典脚本形态、6 枚图标齐活 ──
  try {
    new vm.Script(text, { filename: 'dist/embedded.js' });
  } catch (e) {
    fail('生成的 dist/embedded.js 语法不通过：' + e.message);
  }
  if (/^\s*(?:import|export)\s/m.test(text)) fail('产物里出现了 import/export，必须是经典脚本片段');
  const lost = NAMES.filter((name) => !text.includes("name: '" + name + "'"));
  if (lost.length) fail('产物里缺少图标定义：' + lost.join('、'));
  const leftovers = [...read(ICONS_FILE).matchAll(/\bname:\s*'([^']+)'/g)].map((m) => m[1])
    .filter((name) => !NAMES.includes(name));
  const leaked = leftovers.filter((name) => text.includes("name: '" + name + "'"));
  if (leaked.length) fail('产物里混进了商奕之外的旧图标：' + leaked.join('、'));
  if (!text.includes("window.ShangYiIcons = ShangYiIcons")) fail('产物没有挂 window.ShangYiIcons');

  fs.mkdirSync(path.dirname(OUT_FILE), { recursive: true });
  fs.writeFileSync(OUT_FILE, text);

  const rel = path.relative(ROOT, OUT_FILE);
  console.log('built ' + rel + ' (' + text.split('\n').length + ' 行, ' + Buffer.byteLength(text) + ' 字节)');
  console.log('  图标：' + NAMES.join('、') + '（丢弃旧图标 ' + leftovers.length + ' 枚）');
  console.log('  前置常量：' + (consts.length ? consts.map((line) => line.split(' ')[1].replace('=', '')).join('、') : '无'));
  console.log('  默认主色：' + '墨玉 #1F6B5C' + '；defs id 前缀：sy<单调递增序号>');
}

build();
