    // ──────────────────────────────────────────────────────────────
    // 图标：先查图标集（icons agent 产出的 dist/embedded.js 会挂 window.ShangYiIcons），
    // 有就用真图标（kubai 引擎，悬停形变），没有或挂载失败就用内置占位。
    // 替换点只有一处：window.ShangYiIcons.mount(el, name, { accent })。
    // ──────────────────────────────────────────────────────────────
    var SYIcon = {};

    SYIcon.NAMES = ["弈", "算", "判", "证", "知", "档"];

    SYIcon.hasIconSet = function () {
      return Boolean(
        typeof window !== "undefined" &&
        window.ShangYiIcons &&
        typeof window.ShangYiIcons.mount === "function"
      );
    };

    /** 图标集未就绪时的占位：同一套 16px 网格、currentColor 线性画法。 */
    SYIcon.fallback = function (name, size) {
      var s = size || 16;
      var common = {
        width: s, height: s, viewBox: "0 0 16 16", fill: "none",
        stroke: "currentColor", strokeWidth: 1.2,
        strokeLinecap: "round", strokeLinejoin: "round",
        "aria-hidden": true, style: { display: "block" }
      };
      if (name === "弈") {
        return h("svg", common,
          h("path", { d: "M3 3h10v10H3z", opacity: 0.55 }),
          h("path", { d: "M3 8h10M8 3v10", opacity: 0.55 }),
          h("circle", { cx: 10.5, cy: 5.5, r: 1.6, fill: "currentColor", stroke: "none" }));
      }
      if (name === "算") {
        return h("svg", common,
          h("rect", { x: 2.5, y: 2.5, width: 11, height: 11, rx: 1 }),
          h("path", { d: "M5.5 2.5v11M8 2.5v11M10.5 2.5v11" }),
          h("circle", { cx: 5.5, cy: 5.5, r: 1.1, fill: "currentColor", stroke: "none" }),
          h("circle", { cx: 8, cy: 10, r: 1.1, fill: "currentColor", stroke: "none" }));
      }
      if (name === "判") {
        return h("svg", common,
          h("path", { d: "M8 2.5v11M4.5 13.5h7" }),
          h("path", { d: "M3 5.5h10" }),
          h("path", { d: "M3 5.5l-1.2 2.6a2 2 0 0 0 2.4 0L3 5.5zM13 5.5l-1.2 2.6a2 2 0 0 0 2.4 0L13 5.5z" }));
      }
      if (name === "证") {
        return h("svg", common,
          h("path", { d: "M3.5 2.5h6l3 3v8h-9z" }),
          h("path", { d: "M9.5 2.5v3h3" }),
          h("rect", { x: 5.5, y: 8, width: 4.5, height: 4.5, rx: 0.6, fill: "currentColor", opacity: 0.75, stroke: "none" }));
      }
      if (name === "知") {
        return h("svg", common,
          h("path", { d: "M8 4c-1.4-1.2-3.2-1.6-4.6-1.4v9.2c1.4-.2 3.2.2 4.6 1.4 1.4-1.2 3.2-1.6 4.6-1.4V2.6C11.2 2.4 9.4 2.8 8 4z" }),
          h("path", { d: "M8 4v9.2" }));
      }
      if (name === "档") {
        return h("svg", common,
          h("path", { d: "M2.5 4.5h11v8h-11z" }),
          h("path", { d: "M2.5 4.5l1.2-2h8.6l1.2 2" }),
          h("path", { d: "M6.5 7.5h3" }));
      }
      // 兜底：一枚方章
      return h("svg", common,
        h("rect", { x: 3, y: 3, width: 10, height: 10, rx: 1 }),
        h("rect", { x: 5, y: 5, width: 6, height: 6, rx: 0.5, opacity: 0.5 }));
    };

    /** 统一图标入口。name 取 NAMES；颜色跟随父级 currentColor。 */
    function Icon(props) {
      var name = props.name || "弈";
      var size = props.size || 16;
      var failedState = React.useState(false);
      var failed = failedState[0];
      var setFailed = failedState[1];
      var hostRef = React.useRef(null);

      React.useEffect(function () {
        if (failed || !SYIcon.hasIconSet()) return undefined;
        var el = hostRef.current;
        if (!el) return undefined;
        try {
          window.ShangYiIcons.mount(el, name, props.accent ? { accent: props.accent } : undefined);
        } catch (e) {
          setFailed(true);
        }
        return function () { if (el) el.textContent = ""; };
      }, [name, props.accent, failed]);

      if (!failed && SYIcon.hasIconSet()) {
        // 有图标集：留一个空壳，mount 负责往里面画（key 带名字，换名字即换新节点）。
        return h("span", {
          key: "iconset:" + name,
          ref: hostRef,
          style: { display: "inline-flex", width: size, height: size, alignItems: "center", justifyContent: "center" },
          "aria-hidden": true
        });
      }
      return SYIcon.fallback(name, size);
    }
