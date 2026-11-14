// 商奕领域卡片 —— DSH Web GUI 的浏览器半（手写 bundle，无构建步骤）。
//
// 数据来源：GET /api/shangyi/cards?case=latest（Host 半注册，经 DSH 浏览器鉴权）。
// Host 半把 $SHANGYI_HOME/cases/<日期>/<case_id>/case.jsonl 投影成结构化 JSON；
// 前端**只展示**，不重算、不编造数值（商奕铁律 1：数字只来自确定性代码）。
//
// 为什么手写而不是打包：DSH 客户端插件契约就是「调用 window.__ModuleLoader__.load
// 的文件，factory 里用 require() 取平台模块」，第三方插件产物同形。零构建让改动
// 刷新即生效；等 UI 稳定再上打包。
window.__ModuleLoader__.load({
  id: "dsh-plugin-shangyi",
  factory: function (require) {
    var React = require("react");
    var ReactDOM = require("react-dom");
    var h = React.createElement;

    // ── 样式（全部跟随 --dsw-alias-* 主题令牌，与宣纸·墨玉工作台一致） ──
    var PANEL = {
      position: "fixed", top: 0, right: 0, bottom: 0, width: "min(440px, 94vw)",
      zIndex: 2147483000, background: "var(--dsw-alias-bg-base, #F7F4EC)", color: "var(--dsw-alias-label-primary, #1D1E1C)",
      borderLeft: "1px solid var(--dsw-alias-border-l1, #E3DDD0)", padding: "16px 16px 20px",
      overflowY: "auto",
      font: "13px/1.6 -apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', sans-serif"
    };
    var CARD = { border: "1px solid var(--dsw-alias-border-l1, #E3DDD0)", borderRadius: 8, padding: "12px 12px 10px", marginBottom: 12, background: "var(--dsw-alias-bg-layer-1, #EFECE3)" };
    var H2 = { margin: "0 0 8px", fontSize: 13, fontWeight: 600, color: "var(--dsw-alias-label-primary, #1D1E1C)" };
    var TAG = { display: "inline-block", marginLeft: 6, padding: "0 6px", borderRadius: 8, fontSize: 10, lineHeight: "16px", border: "1px solid var(--dsw-alias-border-l2, #D3CCC0)", color: "var(--dsw-alias-label-secondary, #555752)", fontWeight: 400 };
    var TAG_OK = Object.assign({}, TAG, { border: "1px solid var(--dsw-alias-state-success-primary, #1F6B5C)", color: "var(--dsw-alias-state-success-primary, #1F6B5C)" });
    var TAG_REAL = Object.assign({}, TAG, { border: "1px solid var(--dsw-alias-brand-primary, #1F6B5C)", color: "var(--dsw-alias-brand-primary, #1F6B5C)" });
    var CELL = { padding: "6px 8px", border: "1px solid var(--dsw-alias-border-l1, #E3DDD0)", borderRadius: 8, minWidth: 92, verticalAlign: "top" };
    var BTN = { height: 28, padding: "0 10px", borderRadius: 8, border: "1px solid var(--dsw-alias-border-l2, #D3CCC0)", background: "var(--dsw-alias-bg-base, #F7F4EC)", color: "var(--dsw-alias-label-primary, #1D1E1C)", cursor: "pointer", font: "inherit", fontSize: 12 };
    var MUTED = { color: "var(--dsw-alias-label-secondary, #555752)" };
    var SMALL = { color: "var(--dsw-alias-label-tertiary, #7A7C76)", fontSize: 11 };

    function fmt(n) { return typeof n === "number" ? n.toLocaleString("zh-CN") : String(n); }

    var PHASE_ZH = {
      intake: "受理", intel: "情报", model_build: "建模", solve: "求解",
      select: "选择", consequence: "后果", discover: "发现", recommend: "建议",
      report: "报告", archived: "归档"
    };
    var MODE_ZH = { scenario: "情景推演", whatif: "后果推演", discover: "策略发现", consult: "咨询", research: "研究", cost: "费用测算" };
    var PLAYBOOK_ZH = { pricing_response: "调价 / 促销响应", market_entry: "市场进入", negotiation: "谈判对策", competitor_intel: "竞对情报" };
    var CP_ZH = { cp1: "关口 1 · 立案边界（cp1）", cp2: "关口 2 · 博弈模型与证据（cp2）", cp3: "关口 3 · 最终建议核准（cp3）" };
    var STATUS_ZH = { running: "未收尾", waiting: "待确认", done: "已完成", stuck: "卡住" };
    var ROUTE_ZH = { SupportEnumeration: "支持枚举（精确）", LemkeHowson: "Lemke–Howson（交叉检查）", Analytic: "解析求根" };
    var EQ_KIND_ZH = { Pure: "纯策略", pure: "纯策略", Mixed: "混合策略", mixed: "混合策略" };

    function zhCompleteness(v) {
      if (!v) return "—";
      if (v === "Complete") return "完备";
      if (v === "Partial") return "不完备";
      if (v === "NotGuaranteed") return "不保证完备";
      var s = String(v);
      if (s.indexOf("Partial") === 0) {
        var m = s.match(/unsupported:\s*(\d+),\s*underdetermined:\s*(\d+),\s*ghost:\s*(\d+)/);
        return m ? ("部分完备（超范围 " + m[1] + " · 欠定 " + m[2] + " · 幽灵 " + m[3] + "）") : "部分完备";
      }
      if (s.indexOf("NotGuaranteed") === 0) {
        var am = s.match(/algorithm:\s*"([^"]+)"/);
        return am ? ("不保证完备（" + am[1] + "）") : "不保证完备";
      }
      return s;
    }

    function zhConfidence(conf) {
      if (!conf) return "本案例未产出选择分布（只到 solve 阶段）";
      if (conf.PriorUncalibrated) {
        return "权重未校准（样本 " + conf.PriorUncalibrated.samples + "/" + conf.PriorUncalibrated.needed + "）";
      }
      if (conf.Calibrated) {
        var c = conf.Calibrated;
        var extra = typeof c.mean_log_loss === "number" ? (" · 平均对数损失 " + c.mean_log_loss.toFixed(3)) : "";
        return "已校准（样本 " + c.samples + " · 筛选器 " + (Array.isArray(c.selectors) ? c.selectors.length : 0) + extra + "）";
      }
      return JSON.stringify(conf);
    }

    function Card(props) {
      return h("section", { style: CARD },
        h("h2", { style: H2 }, props.title, props.tag),
        props.children);
    }

    function MetaCard(props) {
      var m = props.meta;
      var modePb = [MODE_ZH[m.mode] || m.mode, PLAYBOOK_ZH[m.playbook] || m.playbook].filter(Boolean).join(" / ") || "—";
      var phases = (m.phases && m.phases.length)
        ? m.phases.map(function (p) { return PHASE_ZH[p] || p; }).join(" → ")
        : "（本案例日志无阶段事件）";
      var cps = (m.checkpoints && m.checkpoints.length)
        ? m.checkpoints.map(function (c) { return CP_ZH[c] || c; }).join("、")
        : "—";
      var rows = [
        ["案例", m.case_id],
        ["模式 / 剧本", modePb],
        ["阶段", phases],
        ["确认点", cps],
        ["轮次 / 工具调用", fmt(m.turns) + " / " + fmt(m.tools)],
        ["token（输入+输出）", fmt(m.tokens)],
        ["事件数", fmt(m.events)]
      ];
      return h(Card, { title: "案例概况", tag: h("span", { style: TAG_REAL }, "真实数据") },
        h("table", { style: { borderCollapse: "collapse", width: "100%" } },
          h("tbody", null, rows.map(function (r, i) {
            return h("tr", { key: i },
              h("td", { style: { color: "var(--dsw-alias-label-secondary, #555752)", padding: "2px 8px 2px 0", whiteSpace: "nowrap", verticalAlign: "top" } }, r[0]),
              h("td", { style: { padding: "2px 0", wordBreak: "break-word" } }, r[1]));
          }))));
    }

    function MatrixCard(props) {
      var mx = props.matrix;
      if (!mx) return h(Card, { title: "支付矩阵" }, h("div", { style: MUTED }, "本案例没有产出博弈结构（可能是研究/费用任务，或建模未完成）。"));
      var axis = mx.players.slice(0, 2);
      var head = h("tr", null, h("th", null), mx.cols.map(function (c, i) {
        return h("th", { key: i, style: { padding: "4px 6px", color: "var(--dsw-alias-label-secondary, #555752)", fontWeight: 500, fontSize: 11, verticalAlign: "bottom" } }, c);
      }));
      var body = mx.grid.map(function (row, ri) {
        return h("tr", { key: ri },
          h("th", { style: { padding: "4px 6px", color: "var(--dsw-alias-label-secondary, #555752)", fontWeight: 500, fontSize: 11, textAlign: "left" } }, row.strategy),
          row.cells.map(function (cell, ci) {
            return h("td", { key: ci, style: CELL },
              cell.cells.filter(function (c) { return axis.some(function (p) { return p.name === c.player; }); })
                .map(function (c, k) {
                  return h("div", { key: k, style: { fontSize: 11 } },
                    h("span", { style: { color: "var(--dsw-alias-label-tertiary, #7A7C76)" } }, c.player + " "), c.expr);
                }));
          }));
      });
      var notes = [];
      if (mx.players.length > 2) notes.push("另有玩家：" + mx.players.slice(2).map(function (p) { return p.name; }).join("、") + "（只在部分格子给出支付）");
      if (mx.params) notes.push("参数 " + mx.params + " 个（支付值以 ⟨参数⟩ 引用表示，终值由引擎求解，不在前端计算）");
      return h(Card, { title: "支付矩阵", tag: h("span", { style: TAG_REAL }, mx.game_id || "真实数据") },
        h("div", { style: { overflowX: "auto" } },
          h("table", { style: { borderCollapse: "separate", borderSpacing: 5 } },
            h("thead", null, head), h("tbody", null, body))),
        notes.length ? h("div", { style: Object.assign({ marginTop: 6 }, MUTED) }, notes.join("；")) : null);
    }

    function bar(value, color) {
      return h("div", { style: { background: "var(--dsw-alias-bg-layer-3, #E6E0D3)", borderRadius: 999, height: 8, overflow: "hidden" } },
        h("div", { style: { width: Math.max(0, Math.min(100, Math.round(value * 100))) + "%", height: "100%", background: color || "var(--dsw-alias-brand-primary, #1F6B5C)" } }));
    }

    function EquilibriumCard(props) {
      var s = props.solve, sel = props.selection;
      if (!s || !s.equilibria.length) {
        return h(Card, { title: "均衡" }, h("div", { style: MUTED }, "本案例日志里没有成功的 solve_equilibria 结果。"));
      }
      var probs = {};
      (sel && sel.entries || []).forEach(function (e) { probs[e.equilibrium_id] = e.prob; });
      var confText = zhConfidence(sel && sel.confidence);
      return h(Card, { title: "均衡与选择", tag: h("span", { style: TAG_REAL }, ROUTE_ZH[s.route] || s.route || "真实数据") },
        s.equilibria.map(function (e) {
          var p = probs[e.id];
          return h("div", { key: e.id, style: { marginBottom: 9 } },
            h("div", { style: { display: "flex", justifyContent: "space-between", gap: 8 } },
              h("span", null, e.id + " · " + (EQ_KIND_ZH[e.kind] || e.kind) + (e.payoffs ? " · 支付 [" + e.payoffs.join(", ") + "]" : "")),
              typeof p === "number" ? h("span", { style: MUTED }, Math.round(p * 100) + "%") : null),
            typeof p === "number" ? bar(p) : null);
        }),
        h("div", { style: Object.assign({ marginTop: 4 }, MUTED) }, confText),
        h("div", { style: SMALL }, "完整度：" + zhCompleteness(s.completeness)));
    }

    function FailureCard(props) {
      var fs = props.failures || [];
      if (!fs.length) return h(Card, { title: "工具失败 / 闸门拒绝" }, h("div", { style: MUTED }, "本案例没有失败或拒绝记录。"));
      var label = { failed: "失败", blocked: "未取得", unknown: "结局未知" };
      return h(Card, { title: "工具失败 / 闸门拒绝", tag: h("span", { style: TAG }, "真实原文") },
        fs.slice().reverse().map(function (f, i) {
          var isErr = f.status === "failed";
          var toneColor = isErr ? "var(--dsw-alias-state-error-primary, #A83E30)" : "var(--dsw-alias-label-secondary, #555752)";
          var borderColor = isErr ? "var(--dsw-alias-state-error-primary, #A83E30)" : "var(--dsw-alias-border-l2, #D3CCC0)";
          return h("div", { key: i, style: { marginBottom: 8 } },
            h("div", { style: { color: toneColor, fontSize: 11 } }, f.tool + " · " + (label[f.status] || f.status)),
            h("pre", { style: { margin: "3px 0 0", whiteSpace: "pre-wrap", wordBreak: "break-word", background: "var(--dsw-alias-bg-layer-2, #E9E4D8)", border: "1px solid " + borderColor, borderRadius: 8, padding: 8, color: "var(--dsw-alias-label-primary, #1D1E1C)", fontSize: 11 } },
              f.detail.length > 260 ? f.detail.slice(0, 260) + "…" : f.detail));
        }));
    }

    function CasePicker(props) {
      var cases = props.cases || [];
      if (cases.length < 2) return null;
      return h("select", {
        value: props.value || "",
        title: "选择案例（来自 $SHANGYI_HOME/cases）",
        onChange: function (e) { props.onChange(e.target.value); },
        style: Object.assign({}, BTN, { maxWidth: "60%", padding: "0 6px" })
      }, cases.map(function (c) {
        var flag = c.hasMatrix || c.hasSolve ? " [博弈]" : "";
        var d = c.mtime ? c.mtime.slice(0, 10) : "";
        var modeTxt = MODE_ZH[c.mode] || c.mode || "未知";
        var statusTxt = c.status && STATUS_ZH[c.status] ? (" [" + STATUS_ZH[c.status] + "]") : "";
        return h("option", { key: c.caseId, value: c.caseId }, d + " " + modeTxt + " · " + c.caseId.replace(/^case-/, "") + flag + statusTxt);
      }));
    }

    function Drawer(props) {
      var st = React.useState({ phase: "loading", data: null, error: null, cases: [], health: null });
      var state = st[0], setState = st[1];

      var load = React.useCallback(function (ref) {
        setState(function (prev) { return { phase: "loading", data: prev.data, error: null, cases: prev.cases, health: prev.health }; });
        fetch("/api/shangyi/cards?case=" + encodeURIComponent(ref || "latest-cards"), { headers: { accept: "application/json" }, cache: "no-store" })
          .then(function (r) { if (!r.ok) throw new Error("HTTP " + r.status); return r.json(); })
          .then(function (j) {
            if (!j || j.ok !== true) throw new Error((j && j.error) || "接口返回 ok=false");
            setState(function (prev) { return { phase: "ok", data: j, error: null, cases: prev.cases, health: prev.health }; });
          })
          .catch(function (e) { setState(function (prev) { return { phase: "error", data: null, error: String(e && e.message || e), cases: prev.cases, health: prev.health }; }); });
      }, []);

      React.useEffect(function () {
        load("latest-cards");
        fetch("/api/shangyi/cases", { headers: { accept: "application/json" }, cache: "no-store" })
          .then(function (r) { return r.json(); })
          .then(function (j) { if (j && j.ok && Array.isArray(j.cases)) setState(function (prev) { return Object.assign({}, prev, { cases: j.cases }); }); })
          .catch(function () { /* 选择器失败不影响卡片 */ });
        fetch("/api/shangyi/health", { headers: { accept: "application/json" }, cache: "no-store" })
          .then(function (r) { return r.json(); })
          .then(function (hJson) { if (hJson) setState(function (prev) { return Object.assign({}, prev, { health: hJson }); }); })
          .catch(function () { /* 健康检查失败不阻断列表 */ });
      }, [load]);

      var healthAlert = state.health && state.health.ok === false
        ? h("div", { style: { marginBottom: 10, padding: "8px 10px", borderRadius: 8, border: "1px solid var(--dsw-alias-state-error-primary, #A83E30)", background: "var(--dsw-alias-bg-layer-2, #E9E4D8)", color: "var(--dsw-alias-state-error-primary, #A83E30)", fontSize: 12 } },
            h("div", { style: { fontWeight: 600, marginBottom: 4 } }, "⚠ 商奕确定性求解内核未就绪（请勿无工具盲聊）"),
            ((state.health.problems || []).map(function (p, idx) {
              return h("div", { key: idx, style: { color: "var(--dsw-alias-label-primary, #1D1E1C)", marginTop: 4 } },
                p.title + " — " + p.detail + ((p.fixCommands && p.fixCommands[0]) ? (" ｜ 建议执行：" + p.fixCommands[0]) : ""));
            })))
        : null;

      var body;
      if (state.phase === "loading" && !state.data) {
        body = h(Card, { title: "读取中" }, h("div", { style: MUTED }, "正在读取商奕案例日志（case.jsonl）…"));
      } else if (state.phase === "error") {
        body = h(Card, { title: "接口不可用" },
          h("div", { style: { color: "var(--dsw-alias-state-error-primary, #A83E30)" } }, state.error),
          h("div", { style: Object.assign({ marginTop: 6 }, MUTED) },
            "卡片没有退化成编造的样例：宁可不显示，也不把样例讲成真结果。"),
          h("button", { style: Object.assign({ marginTop: 8 }, BTN), onClick: function () { load("latest-cards"); } }, "重试"));
      } else {
        var d = state.data;
        var badAlert = d.source && d.source.badLines > 0
          ? h("div", { style: { marginBottom: 10, padding: "6px 10px", borderRadius: 8, border: "1px solid var(--dsw-alias-state-warn-primary, #986618)", color: "var(--dsw-alias-state-warn-primary, #986618)", fontSize: 12 } },
              "日志存在坏行 " + fmt(d.source.badLines) + " 条（已跳过，未纳入卡片数字）。")
          : null;
        body = h(React.Fragment, null,
          badAlert,
          h(MetaCard, { meta: d.meta }),
          h(MatrixCard, { matrix: d.matrix }),
          h(EquilibriumCard, { solve: d.solve, selection: d.selection }),
          h(FailureCard, { failures: d.failures }));
      }

      return h("div", { style: PANEL, role: "dialog", "aria-label": "商奕领域卡片" },
        h("div", { style: { display: "flex", alignItems: "center", gap: 6, marginBottom: 8, flexWrap: "wrap" } },
          h("strong", { style: { fontSize: 14 } }, "商奕 · 领域卡片"),
          h("span", { style: { flex: 1 } }),
          h("button", { style: BTN, onClick: function () { load(state.data ? state.data.source.caseId : "latest-cards"); }, title: "重新读取案例日志" }, "刷新"),
          h("button", { style: BTN, onClick: props.onClose, title: "关闭" }, "关闭")),
        h("div", { style: { display: "flex", alignItems: "center", gap: 6, marginBottom: 10 } },
          h("span", { style: MUTED }, "案例"),
          h(CasePicker, { cases: state.cases, value: state.data ? state.data.source.caseId : "", onChange: load })),
        h("div", { style: Object.assign({ marginBottom: 10 }, MUTED) },
          "数据来自商奕案例日志（case.jsonl）的只读投影；前端只展示，不重算、不编造数字。"),
        healthAlert,
        body,
        state.data ? h("div", { style: SMALL },
          "来源：" + state.data.source.path + "（" + state.data.source.caseId + "，请求 " + state.data.requested + "）") : null,
        h("div", { style: Object.assign({ marginTop: 10 }, SMALL) }, "dsh-plugin-shangyi 0.1.0 · 零构建 bundle"));
    }

    function createTrigger(ctx) {
      return function Trigger() {
        var s = React.useState(false);
        var open = s[0], setOpen = s[1];
        function handleClick() {
          if (typeof window !== "undefined" && window.__SHANGYI_WORKBENCH_READY__ &&
            ctx && ctx.layout && typeof ctx.layout.selectPanel === "function") {
            ctx.layout.selectPanel("shangyi-workbench");
            return;
          }
          setOpen(!open);
        }
        return h(React.Fragment, null,
          h("button", { type: "button", style: BTN, title: "打开商奕工作台", onClick: handleClick }, "商奕工作台"),
          open ? ReactDOM.createPortal(h(Drawer, { onClose: function () { setOpen(false); } }), document.body) : null);
      };
    }

    function apply(ctx) {
      var Trigger = createTrigger(ctx);
      ctx.slots.inject("conversation.session.header.utilities", function () {
        return ctx.slots.register({
          name: "conversation.session.header.utilities",
          id: "shangyi:cards",
          order: 20,
          registrant: "dsh-plugin-shangyi"
        }, Trigger);
      });
    }

    return { name: "dsh-plugin-shangyi", inject: ["slots", "layout"], apply: apply };
  }
});
