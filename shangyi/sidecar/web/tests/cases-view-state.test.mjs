import test from 'node:test';
import assert from 'node:assert/strict';
import {api} from '../js/api.js';
import {render as renderCases,resolveViewState,restoreKind,destroy} from '../js/views/cases.js';
import {MODE_LABEL,TASK_FILTERS} from '../js/util.js';

// UI-005：从历史详情返回列表后，搜索词、筛选与滚动位置都不该丢。
// 原形态：render() 每次从空 filters 起步，用户得重新打字、重新筛、重新滚。

test('显式传入的 filters 优先于上次留在标签页里的状态', () => {
  const saved = { kind: 'cost', query: '续租' };
  assert.deepEqual(resolveViewState({ kind: 'research', query: '开店' }, saved), { kind: 'research', query: '开店' });
  assert.deepEqual(resolveViewState({}, saved), { kind: 'cost', query: '续租' });
  assert.deepEqual(resolveViewState({}, null), { kind: 'all', query: '' });
  assert.deepEqual(resolveViewState(), { kind: 'all', query: '' });
});

test('显式传空串也算数——"重新加载"按钮传的是当前页面的值', () => {
  assert.deepEqual(resolveViewState({ query: '' }, { query: '续租' }), { kind: 'all', query: '' });
});

test('存下来的任务 id 不认识时退回"全部"，旧的分析模式 id 也不认', () => {
  assert.equal(restoreKind('all'), 'all');
  assert.equal(restoreKind('cost'), 'cost');
  assert.equal(restoreKind('deleted_kind'), 'all');
  assert.equal(restoreKind(undefined), 'all');
  assert.equal(restoreKind(null), 'all');
  // 旧版存的是分析模式 id（scenario/whatif/...）。认了就会把「费用整理」筛成
  // 「试做法」，所以**故意不认**——旧值一律退回「全部」。
  assert.equal(restoreKind('whatif'), 'all');
  assert.equal(restoreKind('scenario'), 'all');
  // 每个合法任务都要能恢复回来（将来加新任务时这条会提醒补）
  for (const [id] of TASK_FILTERS) assert.equal(restoreKind(id), id);
});

// ── 带 DOM 的恢复链路 ─────────────────────────────────────────────────────
function testSurface() {
  const elements = new Map();
  const node = (id) => {
    if (!elements.has(id)) elements.set(id, {
      id, innerHTML: '', textContent: '', value: '', disabled: false, hidden: false,
      scrollTop: 0, scrollHeight: 0, addEventListener() {}, setAttribute() {}, focus() {},
    });
    return elements.get(id);
  };
  const view = {
    innerHTML: '',
    querySelector: node,
    querySelectorAll: () => [],
  };
  return { node, view, document: { getElementById: node, createElement: () => ({ click() {}, style: {} }) } };
}

function fakeStorage() {
  const store = new Map();
  return {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, String(v)),
    removeItem: (k) => store.delete(k),
    _store: store,
  };
}

const caseRow = (id, extra = {}) => ({
  date: '2026-10-02', case_id: 'case-' + id, question: '问题 ' + id, mtime: 100,
  mode: 'scenario', playbook_id: 'negotiation', ...extra,
});
const chatRow = (id, extra = {}) => ({
  id: 'chat-' + id, title: '对话 ' + id, initial: '对话 ' + id, topic: 'negotiation',
  mode: 'whatif', created_at: 200, status: 'waiting', alive: true, case: null, ...extra,
});

async function withSurface(saved, fn) {
  const originals = { document: globalThis.document, sessionStorage: globalThis.sessionStorage, ...api };
  const surface = testSurface();
  globalThis.document = surface.document;
  globalThis.sessionStorage = fakeStorage();
  if (saved) globalThis.sessionStorage.setItem('shangyi-cases-view-v1', JSON.stringify(saved));
  try {
    await fn(surface);
  } finally {
    destroy();
    globalThis.document = originals.document;
    globalThis.sessionStorage = originals.sessionStorage;
    for (const key of Object.keys(api)) api[key] = originals[key];
  }
}

test('返回列表时恢复搜索词与筛选，输入框与结果都回到上次的样子', async () => {
  await withSurface({ kind: 'cost', query: '续租' }, async (surface) => {
    // 这条对话的 task_scope 是 cost（一次费用整理），kind 才对得上存档里的筛选
    api.cases = async () => ({ cases: [caseRow('a', { question: '别的事项' })] });
    api.chats = async () => ({ chats: [chatRow('b', { task_scope: 'cost', initial: '续租谈判' })] });
    api.models = async () => ({ models: [] });
    await renderCases(surface.view, { isCurrent: () => true });
    // 搜索词是写进 innerHTML 的 value 属性里的（真实浏览器里就是输入框的内容）
    assert.match(surface.view.innerHTML, /id="case-search"[^>]*value="\u7eed\u79df"/, '搜索词要填回输入框');
    assert.match(surface.node('#case-results').innerHTML, /#\/chat\/chat-b/, '筛选结果要是上次那一套');
    assert.ok(!surface.node('#case-results').innerHTML.includes('别的事项'),
      '不匹配任务类型的记录不该混进来');
  });
});

test('离开列表时存下滚动位置，返回时滚回原处', async () => {
  await withSurface({ scrollTop: 480 }, async (surface) => {
    api.cases = async () => ({ cases: [caseRow('a')] });
    api.chats = async () => ({ chats: [] });
    api.models = async () => ({ models: [] });
    surface.node('view').scrollHeight = 2000;   // 真实页面有内容高度；桩里要给出，否则夹取到 0
    await renderCases(surface.view, { isCurrent: () => true });
    assert.equal(surface.node('view').scrollTop, 480, '返回列表要滚回原来那条附近');

    // 离开：把当前滚动位置写回去
    surface.node('view').scrollTop = 910;
    destroy();
    const saved = JSON.parse(sessionStorage.getItem('shangyi-cases-view-v1'));
    assert.equal(saved.scrollTop, 910);
  });
});

test('坏 JSON 的视图状态当没有，不影响打开列表', async () => {
  const originals = { document: globalThis.document, sessionStorage: globalThis.sessionStorage, ...api };
  const surface = testSurface();
  globalThis.document = surface.document;
  globalThis.sessionStorage = fakeStorage();
  globalThis.sessionStorage.setItem('shangyi-cases-view-v1', '{oops');
  try {
    api.cases = async () => ({ cases: [caseRow('a')] });
    api.chats = async () => ({ chats: [] });
    api.models = async () => ({ models: [] });
    await renderCases(surface.view, { isCurrent: () => true });
    assert.equal(surface.node('#case-search').value, '', '坏状态不该把搜索框填坏');
    assert.match(surface.node('#case-results').innerHTML, /问题 a/);
  } finally {
    destroy();
    globalThis.document = originals.document;
    globalThis.sessionStorage = originals.sessionStorage;
    for (const key of Object.keys(api)) api[key] = originals[key];
  }
});
