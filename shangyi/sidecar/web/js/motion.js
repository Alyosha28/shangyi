// @ts-check
// 动效封装：Motion 的**唯一**入口。
//
// 为什么要有这层：
//   1. `styles.css` 里那条 `prefers-reduced-motion` 兜底只作用于 CSS，
//      管不到 JS 驱动的动画；这里补上另一半，否则系统开启减弱动效的用户
//      仍会看到页面自己动。
//   2. `vendor/motion.min.js` 是 UMD bundle，靠 `index.html` 里的普通
//      `<script>` 挂出全局 `Motion`。测试环境（`node --test`）没有这个
//      bundle，必须能静默降级，而不是在模块加载时就炸掉——测试里用的是
//      手搓的 `globalThis.document` 桩，跑不了真实动画。
//   3. 本项目**刻意不引入前端构建**，所以这里读全局而不是 `import`。
//      若将来真的加了打包器，只需把 `engine()` 里的取值改成
//      `import * as M from 'motion'`——上层调用点不用动。
//
// 约束（改动前务必读完）：
//   - 只对**跨轮询存活**的容器动效：路由的 `mount` 节点（`app.js` 建、
//     换路由时整体丢弃）。绝不对会被 `innerHTML` 冲掉的节点直接动画，
//     否则动画中途被销毁会闪烁，transform 还可能扰乱
//     `views/chat.js` 的 `preserveScroll` 滚动锚定。
//   - 只动 `opacity` / `transform`：两者不参与布局，不改变 scrollHeight。

/** 取引擎。延迟到调用时读，便于测试注入或替换。 */
function engine() {
  const M = globalThis.Motion;
  return M && typeof M.animate === 'function' ? M : null;
}

/** 系统是否要求减弱动效。缺 matchMedia 的环境按「减弱」处理，宁可不动。 */
function reduced() {
  const mq = globalThis.matchMedia;
  if (typeof mq !== 'function') return true;
  try {
    return mq('(prefers-reduced-motion: reduce)').matches === true;
  } catch {
    return true;
  }
}

/** 引擎就绪且允许动效。任一不满足则所有入口静默返回，页面行为完全不变。 */
function ready() {
  return !!engine() && typeof document !== 'undefined' && !reduced();
}

/** 挑出实际元素：过滤空值与非 Element（测试桩里常有）。 */
function nodes(root, selector) {
  if (!root || typeof root.querySelectorAll !== 'function') return [];
  let found;
  try {
    found = root.querySelectorAll(selector);
  } catch {
    return [];
  }
  return Array.from(found || []);
}

/**
 * 路由视图入场。换路由时对新 mount 做一次极短的淡入+下沉。
 * 只在导航瞬间跑一次，与轮询重绘无关，因此不会和 preserveScroll 打架。
 */
export function enterView(el) {
  if (!ready() || !el) return;
  try {
    engine().animate(
      el,
      { opacity: [0, 1], transform: ['translateY(6px)', 'translateY(0px)'] },
      { duration: 0.22, easing: 'ease-out' },
    );
  } catch {
    /* 动效失败不影响内容 */
  }
}

/**
 * 在渲染开始前把视图按住（opacity:0），等内容就位再由 `enterView` 放开。
 *
 * 必须成对使用：只有 `holdView` 返回 true 才需要 `enterView`。若引擎
 * 不可用，`holdView` 返回 false 且**不动 opacity**，否则整页会永久停在
 * 不可见状态——降级路径必须保证「没动效也能正常用」。
 */
export function holdView(el) {
  if (!ready() || !el) return false;
  try {
    el.style.opacity = '0';
    return true;
  } catch {
    return false;
  }
}

/**
 * 列表一次性入场（错峰淡入）。只适合**首屏**渲染——
 * 搜索框每敲一个字就重绘的列表，重复 animate 会一直闪。
 */
export function staggerList(root, selector) {
  if (!ready()) return;
  const list = nodes(root, selector);
  if (list.length < 2) return;
  try {
    engine().animate(
      list,
      { opacity: [0, 1], transform: ['translateY(8px)', 'translateY(0px)'] },
      { delay: engine().stagger(0.035), duration: 0.26, easing: 'ease-out' },
    );
  } catch {
    /* 同上 */
  }
}

/**
 * 只给**新出现**的条目入场，用于会被轮询整体重绘的列表
 * （`views/activity.js` 每 2.5s 替换一次 `#runs`）。
 * 靠 key 判重，避免每次轮询都把整屏重新闪一遍。
 */
export function enterNew(root, selector, keyAttr = 'data-item') {
  if (!ready()) return;
  const list = nodes(root, selector);
  if (!list.length) return;
  const seen = root.__sySeen || (root.__sySeen = new Set());
  const fresh = list.filter((el) => {
    const key = el.getAttribute ? el.getAttribute(keyAttr) : null;
    if (!key || seen.has(key)) return false;
    seen.add(key);
    return true;
  });
  // seen 挂在容器上，而容器每次换路由都会被整体丢弃，因此不需要额外的上限。
  if (!fresh.length) return;
  try {
    engine().animate(
      fresh,
      { opacity: [0, 1], transform: ['translateY(6px)', 'translateY(0px)'] },
      { duration: 0.28, easing: 'ease-out' },
    );
  } catch {
    /* 同上 */
  }
}

/**
 * 清掉 `enterNew` 在容器上留下的判重记录。
 * 换上下文时调用（如路由切换清空证据面板），否则新页面里
 * 同名条目会被当成「早就出现过了」而不再入场。
 */
export function resetItems(root) {
  if (root && root.__sySeen) root.__sySeen.clear();
}

/**
 * 轻量入场：**只动 opacity，绝不碰 transform**。
 *
 * ⚠️ 这是给 `#toast` 这类元素用的。`styles.css` 里
 * `#toast{...transform:translateX(-50%)}` 靠 transform 居中，而引擎
 * 写 transform 时会整条覆盖——一旦动画里带上位移或缩放，toast 就会
 * 偏到屏幕右侧去。要用位移，请改用 `enterPop` 并自己保证居中。
 */
export function fadeIn(el, duration = 0.18) {
  if (!ready() || !el) return;
  try {
    engine().animate(el, { opacity: [0, 1] }, { duration, easing: 'ease-out' });
  } catch {
    /* 动效失败不影响内容 */
  }
}

/**
 * 出场后回调。引擎缺失或系统要求减弱动效时**同步**回调 `done`——
 * 调用方靠它执行 `hidden=true` 之类的收尾，绝不能把 `done` 吞掉，
 * 否则元素会永远留在屏幕上。
 */
export function fadeOut(el, done, duration = 0.18) {
  if (typeof done !== 'function') return;
  if (!ready() || !el) {
    done();
    return;
  }
  try {
    engine().animate(el, { opacity: [1, 0] }, { duration, easing: 'ease-in' }).finished.then(done, done);
  } catch {
    done();
  }
}

/**
 * 状态文字变化时的入场（`#page-status`）。
 * 只在文字**真的变了**的时候调用——轮询每 2.5s 会重设一次同样的文字，
 * 无条件动画会变成一直闪。调用点负责比较。
 */
export function statusIn(el) {
  if (!ready() || !el) return;
  try {
    engine().animate(
      el,
      { opacity: [0, 1], transform: ['translateY(-3px)', 'translateY(0px)'] },
      { duration: 0.24, easing: 'ease-out' },
    );
  } catch {
    /* 同上 */
  }
}

/**
 * 长页面滚动揭示（报告、计算依据、证据链）。进入视口才动，
 * 让用户看到内容是「展开」出来的，而不是本来就铺在那。
 */
export function revealOnScroll(root, selector) {
  if (!ready()) return;
  const list = nodes(root, selector);
  if (!list.length) return;
  const M = engine();
  try {
    for (const el of list) {
      if (el.__syRevealed) continue;
      el.__syRevealed = true;
      M.inView(
        el,
        () => {
          try {
            M.animate(
              el,
              { opacity: [0, 1], transform: ['translateY(10px)', 'translateY(0px)'] },
              { duration: 0.3, easing: 'ease-out' },
            );
          } catch {
            /* 同上 */
          }
        },
        { amount: 0.15 },
      );
    }
  } catch {
    /* 同上 */
  }
}

/** 供测试与调试查询当前是否真的会动。 */
export function motionEnabled() {
  return ready();
}