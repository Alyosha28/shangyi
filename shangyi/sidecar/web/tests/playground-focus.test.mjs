import test from 'node:test';
import assert from 'node:assert/strict';
import {render,updateChoiceName} from '../js/views/playground.js';

// UI-004：改完做法按 Tab，焦点不能落到 BODY。
// 原实现：updateChoiceName 改名后调用 drawChoices()，后者用 innerHTML 重建整组
// 输入框，当前节点被移除 ⇒ 焦点掉到 body，键盘用户没法连续填下一项。
// 修法：只同步依赖名称的零散位置，其它输入框原样留着；真掉了焦点再还回去。
//
// 这里用能记焦点的小 DOM 复现那条路径（不测 CSS、不测真键盘；真 Tab 行为已用
// headless Chrome 的 Input.dispatchKeyEvent 验过）。

function miniDom() {
  const body = { tagName: 'BODY', className: '', dataset: {}, focus() {}, setSelectionRange() {} };
  const state = { activeElement: body };
  const makeInput = (cls, side, i, value) => ({
    tagName: 'INPUT', className: cls, dataset: { side: String(side), i: String(i) },
    value, selectionStart: 0, selectionEnd: 0, listeners: {},
    addEventListener(type, fn) { (this.listeners[type] ||= []).push(fn); },
    dispatch(type) { (this.listeners[type] || []).forEach((fn) => fn({ target: this })); },
    focus() { state.activeElement = this; },
    setSelectionRange(a, b) { this.selectionStart = a; this.selectionEnd = b; },
    setAttribute() {}, removeAttribute() {},
  });
  return { state, body, makeInput };
}

test('改名后其它输入框原样保留，焦点掉了会还回原字段', async () => {
  const { state, body, makeInput } = miniDom();
  const choices = [
    makeInput('pg-choice-name', 1, 0, '保持原价'),
    makeInput('pg-choice-name', 1, 1, '跟着降价'),
    makeInput('pg-choice-name', 2, 0, '保持原价'),
    makeInput('pg-choice-name', 2, 1, '降价'),
  ];
  const heads = choices.map((c) => makeInput('strat-name', c.dataset.side, c.dataset.i, c.value));
  const sections = [0, 1, 2, 3].map(() => ({ querySelector: () => ({ set innerHTML(v) {} }) }));
  const outcomes = { querySelectorAll: (sel) => (sel === '.pg-outcome' ? sections : []) };
  const matrix = { querySelectorAll: (sel) => (sel === '.strat-name' ? heads : []) };
  const stub = { innerHTML: '', textContent: '', value: '', hidden: false, classList: { toggle() {} },
    querySelector: () => null, querySelectorAll: () => [], addEventListener() {}, setAttribute() {},
    removeAttribute() {}, prepend() {}, after() {}, closest: () => null, focus() {} };
  const view = {
    innerHTML: '',
    addEventListener() {},
    querySelector: (sel) => {
      if (sel === '#pg-matrix') return matrix;
      if (sel === '#pg-outcomes') return outcomes;
      // .pg-choice-name[data-side="1"][data-i="0"] / .strat-name[...]：按属性找到具体那个输入框
      const attr = /\.(pg-choice-name|strat-name)\[data-side="(\d)"\]\[data-i="(\d)"\]/.exec(sel);
      if (attr) {
        const list = attr[1] === 'pg-choice-name' ? choices : heads;
        return list.find((n) => n.dataset.side === attr[2] && n.dataset.i === attr[3]) || null;
      }
      // 其余（#pg-spec-pre、#pg-result-sub…）给一个能吃赋值/查询的桩
      return stub;
    },
    querySelectorAll: (sel) => (sel === '.pg-choice-name' ? choices : sel === '.strat-name' ? heads : []),
  };
  const saved = { document: globalThis.document, sessionStorage: globalThis.sessionStorage };
  globalThis.document = { getElementById: () => null, createElement: () => ({ style: {} }), body };
  Object.defineProperty(globalThis.document, 'activeElement', { get: () => state.activeElement, configurable: true });
  globalThis.sessionStorage = { getItem: () => null, setItem() {}, removeItem() {} };
  try {
    await render(view, { isCurrent: () => true });
    // 用户在第一个框里打字，然后焦点被打掉（innerHTML 重建的那一瞬间）
    choices[0].value = '暂时维持售价';
    choices[0].selectionStart = 6;
    state.activeElement = body;
    updateChoiceName(view, '1', 0, choices[0]);

    assert.equal(choices[0].value, '暂时维持售价', '改名本身要生效');
    assert.equal(choices[1].value, '跟着降价', '相邻输入框的内容不能被重绘清掉');
    assert.equal(choices[3].value, '降价', '另一侧的做法不能被重绘清掉');
    assert.equal(heads[0].value, '暂时维持售价', '矩阵里同一侧的名字要同步');
    assert.equal(heads[1].value, '跟着降价', '另一侧的名字不受影响');
    assert.equal(state.activeElement, choices[0], '焦点掉了要还回原来那个输入框');
    assert.equal(choices[0].selectionStart, 6, '光标位置也要还回去');
  } finally {
    globalThis.document = saved.document;
    globalThis.sessionStorage = saved.sessionStorage;
  }
});
