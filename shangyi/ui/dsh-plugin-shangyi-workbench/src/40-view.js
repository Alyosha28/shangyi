    // ──────────────────────────────────────────────────────────────
    // 视图层：纯函数。renderPanel(vm) 不读 hooks、不碰 DOM、不发请求，
    // 因此可以在没有浏览器的地方用假 view model 直接跑（见 scripts/verify.mjs）。
    // ──────────────────────────────────────────────────────────────
    var SYView = {};

    var TAB_DEFS = [
      { id: "overview", label: "概览", icon: "弈" },
      { id: "matrix", label: "矩阵", icon: "算" },
      { id: "equilibrium", label: "均衡", icon: "判" },
      { id: "trail", label: "轨迹", icon: "证" }
    ];

    /** 小工具：把可能为 undefined 的东西显示成「—」。 */
    function orDash(v) {
      if (v === null || v === undefined || v === "") return "—";
      return String(v);
    }

    SYView.Tag = function (props) {
      return h("span", { className: "sy-tag " + (props.tone ? "is-" + props.tone : "") }, props.children);
    };

    SYView.Loading = function (props) {
      return h("div", { className: "sy-loading" },
        h("span", { className: "sy-spin", "aria-hidden": true }),
        h("span", null, props.label || "读取中…"));
    };

    SYView.ErrorBox = function (props) {
      return h("div", { className: "sy-error", role: "alert" },
        h("span", { "aria-hidden": true }, "！"),
        h("span", { style: { flex: "1 1 auto" } }, props.message || "读取失败"),
        props.onRetry ? h("button", { type: "button", className: "sy-btn", onClick: props.onRetry }, "重试") : null);
    };

    SYView.Empty = function (props) {
      return h("div", { className: "sy-empty" },
        props.icon ? h("span", { className: "sy-empty-icon" }, h(Icon, { name: props.icon, size: props.iconSize || 32 })) : null,
        h("h2", null, props.title),
        props.body ? h("p", null, props.body) : null,
        props.actions && props.actions.length
          ? h("div", { className: "sy-empty-actions" }, props.actions)
          : null);
    };

    /** 状态 → 标签。没有 status 字段时返回 null（host-data 可能还没补上）。 */
    SYView.statusTag = function (status) {
      if (status === "running") return h(SYView.Tag, { tone: "warn" }, "进行中");
      if (status === "done") return h(SYView.Tag, { tone: "ok" }, "已完成");
      if (status === "stuck") return h(SYView.Tag, { tone: "err" }, "卡住");
      return null;
    };

    SYView.phaseChain = function (cards) {
      var chain = SYData.phaseChain(cards);
      if (!chain.length) {
        return h("div", { className: "sy-note" }, "本案例日志没有阶段事件（旧案例或直接调用工具产生的日志）。");
      }
      var nodes = [];
      for (var i = 0; i < chain.length; i++) {
        if (i > 0) nodes.push(h("span", { key: "sep" + i, className: "sy-sep", "aria-hidden": true }, "→"));
        nodes.push(h("span", {
          key: chain[i].id + i,
          className: "sy-phase" + (chain[i].current ? " is-current" : chain[i].done ? " is-done" : ""),
          title: chain[i].id
        }, chain[i].label));
      }
      return h("div", { className: "sy-chain", role: "list", "aria-label": "阶段链" }, nodes);
    };

    // ── 左栏：案例列表 ──
    SYView.caseItem = function (item, selected, actions) {
      return h("button", {
        key: item.caseId,
        type: "button",
        className: "sy-case",
        "aria-pressed": selected ? "true" : "false",
        onClick: function () { actions.selectCase(item.caseId); }
      },
        h("div", { className: "sy-case-id", title: item.caseId }, SYData.shortCaseId(item.caseId)),
        h("div", { className: "sy-case-meta" },
          h("span", null, SYData.modeLabel(item.mode)),
          item.phase ? h("span", null, "· " + SYData.phaseLabel(item.phase)) : null,
          item.events !== null ? h("span", null, "· " + SYData.fmtNum(item.events) + " 事件") : null,
          item.hasMatrix ? h("span", { className: "sy-tag is-mute" }, "矩阵") : null,
          item.hasSolve ? h("span", { className: "sy-tag is-mute" }, "均衡") : null,
          SYView.statusTag(item.status)
        ));
    };

    SYView.casesColumn = function (vm) {
      var st = vm.cases;
      var body;
      if (st.phase === "loading") {
        body = h("div", { style: { padding: "0 8px" } },
          h("div", { className: "sy-skel" }), h("div", { className: "sy-skel" }), h("div", { className: "sy-skel" }));
      } else if (st.phase === "error") {
        body = h("div", { style: { padding: "0 8px" } },
          h(SYView.ErrorBox, { message: "案例列表读取失败：" + (st.error || "未知错误"), onRetry: vm.actions.refresh }));
      } else if (!vm.filteredCases.length) {
        body = h("div", { style: { padding: "16px 8px" } },
          h("div", { className: "sy-note" }, vm.query ? "没有匹配的案例。清空搜索试试。" : "还没有案例。"));
      } else {
        body = vm.filteredCases.map(function (item) {
          return SYView.caseItem(item, item.caseId === vm.selectedCaseId, vm.actions);
        });
      }
      return h("div", { className: "sy-wb-left" },
        h("div", { className: "sy-search" },
          h("input", {
            type: "search",
            value: vm.query,
            placeholder: "搜索案例 id / 模式",
            "aria-label": "搜索案例",
            onChange: function (e) { vm.actions.setQuery(e.target.value); }
          })),
        h("div", { className: "sy-list", role: "group", "aria-label": "案例列表" }, body),
        h("div", { style: { flex: "none", padding: "8px", borderTop: "1px solid var(--dsw-alias-border-l1)" } },
          h("div", { className: "sy-note" }, "共 " + SYData.fmtNum(vm.cases.items.length) + " 个案例 · 只读日志投影")));
    };

    // ── 中栏：标题 + tabs ──
    SYView.caseHead = function (cards) {
      var m = cards.meta;
      return h("div", { className: "sy-casehead" },
        h("h2", { className: "sy-casename", title: m.caseId || "" }, orDash(m.caseId)),
        h("div", { className: "sy-kv" },
          h("span", null, "模式 ", h("b", null, SYData.modeLabel(m.mode))),
          h("span", null, "剧本 ", h("b", null, orDash(m.playbook))),
          h("span", null, "范围 ", h("b", null, SYData.scopeLabel(m.scope))),
          h("span", null, "创建 ", h("b", null, SYData.fmtTime(m.createdAt))),
          h("span", null, "日志 ", h("b", null, SYData.fmtTime(cards.source.mtime)))));
    };

    SYView.tabs = function (vm) {
      return h("div", { className: "sy-tabs", role: "tablist", "aria-label": "案例视图" },
        TAB_DEFS.map(function (t) {
          return h("button", {
            key: t.id,
            type: "button",
            role: "tab",
            className: "sy-tab",
            "aria-selected": vm.tab === t.id ? "true" : "false",
            onClick: function () { vm.actions.setTab(t.id); }
          },
            h(Icon, { name: t.icon, size: 14 }),
            h("span", null, t.label));
        }));
    };

    // ── 待决关口确认卡（铁律 2：真人确认，写入 user_actor="user"）──
    SYView.checkpointCard = function (vm) {
      var cards = vm.cards && vm.cards.data;
      var cp = cards && cards.pendingCheckpointDetail;
      var cpForm = vm.cpForm || { note: "", phase: "idle", message: null, error: null };
      var notice = cpForm.phase === "done" && cpForm.message
        ? h("div", { className: "sy-status", role: "status" }, cpForm.message)
        : null;
      if (!cp) return notice;

      var busy = cpForm.phase === "submitting";
      var noteId = "sy-cp-note";
      return h("section", { className: "sy-cp-card", "aria-labelledby": "sy-cp-heading" },
        notice,
        h("div", { className: "sy-cp-head" },
          h(Icon, { name: "证", size: 16 }),
          h("h3", { id: "sy-cp-heading", className: "sy-cp-title" }, "待真人确认关口：" + cp.id),
          cp.phase ? h(SYView.Tag, { tone: "warn" }, "阶段：" + SYData.phaseLabel(cp.phase)) : null,
          h(SYView.Tag, { tone: "mute" }, "铁律 2 · actor=user")),
        h("div", { className: "sy-note" },
          "当前阶段挂起等待真人核准；确认或驳回将直接写入 case.jsonl 审计日志，对话中的商奕会在下一轮自动读取推进。"),
        cp.items && cp.items.length
          ? h("ul", { className: "sy-cp-items" },
            cp.items.map(function (item, idx) {
              return h("li", { key: "cpi" + idx }, item);
            }))
          : null,
        cpForm.phase === "error" && cpForm.error
          ? h("div", { style: { marginBottom: "8px" } }, h(SYView.ErrorBox, { message: cpForm.error }))
          : null,
        h("form", {
          className: "sy-cp-form",
          onSubmit: function (e) {
            if (e && typeof e.preventDefault === "function") e.preventDefault();
            if (vm.actions.submitCheckpoint) vm.actions.submitCheckpoint("approve");
          }
        },
          h("label", { className: "sy-cp-label", htmlFor: noteId }, "核准或修订备注（可选，同步记入审计日志）"),
          h("textarea", {
            id: noteId,
            name: "note",
            className: "sy-cp-textarea",
            rows: 2,
            value: cpForm.note || "",
            disabled: busy,
            placeholder: "例如：核对无误，继续建模；或指出需要补充修正的假设项",
            onChange: function (e) {
              if (vm.actions.setCheckpointNote) vm.actions.setCheckpointNote(e.target.value);
            }
          }),
          h("div", { className: "sy-cp-actions" },
            h("button", {
              type: "submit",
              className: "sy-btn is-primary",
              disabled: busy
            }, busy ? "提交中…" : "核准通过"),
            h("button", {
              type: "button",
              className: "sy-btn",
              disabled: busy,
              onClick: function () {
                if (vm.actions.submitCheckpoint) vm.actions.submitCheckpoint("reject");
              }
            }, "驳回修订"))));
    };

    // ── 概览 ──
    SYView.overview = function (vm) {
      var cards = vm.cards.data;
      var m = cards.meta;
      var chain = SYData.phaseChain(cards);
      var current = chain.length ? chain[chain.length - 1] : null;
      return h("div", null,
        SYView.checkpointCard(vm),
        h("div", { className: "sy-sec" },
          h("div", { className: "sy-sec-h" }, "阶段链",
            current ? h("span", { className: "sy-tag is-ok", style: { marginLeft: "8px" } }, "当前：" + current.label) : null),
          SYView.phaseChain(cards)),
        h("div", { className: "sy-stats" },
          h("div", { className: "sy-stat" }, h("div", { className: "sy-k" }, "事件数"), h("div", { className: "sy-v" }, SYData.fmtNum(m.events))),
          h("div", { className: "sy-stat" }, h("div", { className: "sy-k" }, "轮次"), h("div", { className: "sy-v" }, SYData.fmtNum(m.turns))),
          h("div", { className: "sy-stat" }, h("div", { className: "sy-k" }, "工具调用"), h("div", { className: "sy-v" }, SYData.fmtNum(m.tools))),
          h("div", { className: "sy-stat" }, h("div", { className: "sy-k" }, "token（输入+输出）"), h("div", { className: "sy-v" }, SYData.fmtNum(m.tokens)))),
        h("div", { className: "sy-sec" },
          h("div", { className: "sy-sec-h" }, "产物"),
          h("div", { className: "sy-kv" },
            h("span", null, "支付矩阵 ", cards.matrix ? h("b", null, "有") : h("b", null, "无")),
            h("span", null, "均衡 ", (cards.solve && cards.solve.equilibria && cards.solve.equilibria.length ? h("b", null, cards.solve.equilibria.length + " 个") : h("b", null, "无"))),
            h("span", null, "选择分布 ", (cards.selection && cards.selection.entries && cards.selection.entries.length ? h("b", null, "有") : h("b", null, "无"))),
            h("span", null, "确认点 ", (m.checkpoints.length ? h("b", null, m.checkpoints.length + " 个") : h("b", null, "无"))))),
        h("div", { className: "sy-sec" },
          h("div", { className: "sy-sec-h" }, "说明"),
          h("div", { className: "sy-note" },
            "本页全部数字来自商奕案例日志（case.jsonl）的只读投影：前端只展示，不重算、不估算。日志行数 ",
            SYData.fmtNum(m.events),
            m.badLines === null ? "" : "，坏行 " + SYData.fmtNum(cards.source.badLines),
            "。")));
    };

    // ── 矩阵 ──
    SYView.matrixEmpty = function (vm) {
      var alt = null;
      for (var i = 0; i < vm.cases.items.length; i++) {
        var c = vm.cases.items[i];
        if (c.hasMatrix && c.caseId !== vm.selectedCaseId) { alt = c; break; }
      }
      var actions = [];
      if (alt) {
        actions.push(h("button", {
          key: "alt", type: "button", className: "sy-btn is-primary",
          onClick: function () { vm.actions.selectCase(alt.caseId); }
        }, "切到有矩阵的案例"));
      }
      actions.push(h("button", { key: "talk", type: "button", className: "sy-btn", onClick: vm.actions.openConversation }, "去对话里建模"));
      return h(SYView.Empty, {
        icon: "算",
        iconSize: 36,
        title: "本案例没有产出博弈结构",
        body: "日志里没有成功的 gamespec_build 调用。研究 / 费用测算类任务通常不会产出支付矩阵；要建博弈，可在对话里让商奕先建模。",
        actions: actions
      });
    };

    SYView.matrix = function (vm) {
      var cards = vm.cards.data;
      var mx = cards.matrix;
      if (!mx) return SYView.matrixEmpty(vm);
      var players = mx.players || [];
      var cols = mx.cols || [];
      var head = h("tr", null,
        h("th", { scope: "col" }, "行 / 列"),
        cols.map(function (c, i) { return h("th", { key: "c" + i, scope: "col" }, c); }));
      var body = (mx.grid || []).map(function (row, ri) {
        return h("tr", { key: "r" + ri },
          h("th", { scope: "row" }, row.strategy),
          row.cells.map(function (cell, ci) {
            return h("td", { key: "d" + ci },
              cell.cells.length
                ? cell.cells.map(function (line, li) {
                  return h("div", { className: "sy-cell-line", key: "l" + li },
                    h("span", { className: "sy-p" }, line.player),
                    h("span", { className: "sy-expr" }, line.expr));
                })
                : h("span", { className: "sy-note" }, "—"));
          }));
      });
      var extras = [];
      if (players.length > 2) extras.push("另有玩家：" + players.slice(2).map(function (p) { return p.name; }).join("、") + "（只在部分格子给出支付）。");
      return h("div", null,
        h("div", { className: "sy-sec-h", style: { marginTop: "8px" } },
          "支付矩阵",
          h("span", { className: "sy-tag", style: { marginLeft: "8px" } }, mx.game_id || "未命名博弈"),
          mx.form ? h("span", { className: "sy-tag is-mute" }, "形式 " + mx.form) : null,
          h("span", { className: "sy-tag is-mute" }, players.length + " 名玩家"),
          h("span", { className: "sy-tag is-mute" }, (typeof mx.params === "number" ? mx.params : 0) + " 个参数")),
        h("div", { className: "sy-matrix-wrap" },
          h("table", { className: "sy-matrix" },
            h("thead", null, head),
            h("tbody", null, body))),
        h("div", { className: "sy-sec", style: { marginTop: "8px" } },
          h("div", { className: "sy-note" },
            "支付值保持引擎的 ⟨参数⟩ 引用形状：终值由确定性求解器算出，前端不展开、不重算。",
            extras.join(""))));
    };

    // ── 均衡 ──
    SYView.confidenceText = function (confidence) {
      if (!confidence) return "本案例未产出选择分布（只到 solve 阶段）。";
      if (confidence.PriorUncalibrated) {
        var p = confidence.PriorUncalibrated;
        return "选择权重未校准（样本 " + orDash(p.samples) + "/" + orDash(p.needed) + "）。";
      }
      var keys = Object.keys(confidence);
      if (!keys.length) return "选择分布已产出。";
      var parts = [];
      for (var i = 0; i < keys.length && i < 4; i++) {
        var v = confidence[keys[i]];
        parts.push(keys[i] + "=" + (typeof v === "object" ? JSON.stringify(v) : String(v)));
      }
      return "选择置信：" + parts.join("，") + "。";
    };

    SYView.equilibrium = function (vm) {
      var cards = vm.cards.data;
      var solve = cards.solve;
      if (!solve || !Array.isArray(solve.equilibria) || !solve.equilibria.length) {
        return h(SYView.Empty, {
          icon: "判",
          iconSize: 36,
          title: "本案例没有均衡结果",
          body: "日志里没有成功的 solve_equilibria 调用。先建模（gamespec_build）再求解，才会有均衡与选择分布。",
          actions: [h("button", { key: "talk", type: "button", className: "sy-btn is-primary", onClick: vm.actions.openConversation }, "去对话里求解")]
        });
      }
      var probs = SYData.selectionIndex(cards.selection);
      var rows = solve.equilibria.map(function (eq, i) {
        var p = probs[eq.id];
        var pct = typeof p === "number" ? Math.max(0, Math.min(100, Math.round(p * 100))) : null;
        return h("div", { className: "sy-eq", key: eq.id || i },
          h("div", { className: "sy-eq-top" },
            h("span", { className: "sy-eq-id" }, orDash(eq.id)),
            eq.kind ? h("span", { className: "sy-eq-kind" }, eq.kind) : null,
            Array.isArray(eq.payoffs) ? h("span", { className: "sy-eq-pay" }, "支付 [" + eq.payoffs.join(", ") + "]") : null,
            pct === null ? null : h("span", { className: "sy-eq-prob" }, pct + "%")),
          pct === null ? null : h("div", { className: "sy-bar", "aria-hidden": true }, h("i", { style: { width: pct + "%" } })));
      });
      return h("div", null,
        h("div", { className: "sy-sec-h", style: { marginTop: "8px" } },
          "均衡与选择",
          solve.route ? h("span", { className: "sy-tag is-mute" }, "路线 " + solve.route) : null,
          solve.completeness ? h("span", { className: "sy-tag is-mute" }, "完整度 " + solve.completeness) : null,
          h("span", { className: "sy-tag is-mute" }, solve.equilibria.length + " 个均衡")),
        rows,
        h("div", { className: "sy-sec" },
          h("div", { className: "sy-note" }, SYView.confidenceText(cards.selection && cards.selection.confidence))));
    };

    // ── 轨迹 ──
    SYView.trail = function (vm) {
      var cards = vm.cards.data;
      var m = cards.meta;
      var items = [];
      var phases = m.phases || [];
      for (var i = 0; i < phases.length; i++) {
        items.push(h("div", { className: "sy-trail-item", key: "p" + i },
          h("div", { className: "sy-trail-when" }, "阶段 " + (i + 1)),
          h("div", { className: "sy-trail-body" },
            h("div", { className: "sy-trail-tool" }, SYData.phaseLabel(phases[i])),
            h("div", { className: "sy-trail-detail" }, phases[i]))));
      }
      var cps = m.checkpoints || [];
      for (var j = 0; j < cps.length; j++) {
        items.push(h("div", { className: "sy-trail-item", key: "c" + j },
          h("div", { className: "sy-trail-when" }, "确认点 " + (j + 1)),
          h("div", { className: "sy-trail-body" },
            h("div", { className: "sy-trail-tool" }, String(cps[j])),
            h("div", { className: "sy-trail-detail" }, "已答复（checkpoint_answered）"))));
      }
      var fails = cards.failures || [];
      for (var k = 0; k < fails.length; k++) {
        var f = fails[k] || {};
        items.push(h("div", { className: "sy-trail-item", key: "f" + k },
          h("div", { className: "sy-trail-when" }, "失败"),
          h("div", { className: "sy-trail-body" },
            h("div", { className: "sy-trail-tool" }, orDash(f.tool)),
            h("div", { className: "sy-trail-detail" }, orDash(f.status) + " · " + orDash(f.detail)))));
      }
      return h("div", null,
        h("div", { className: "sy-sec-h", style: { marginTop: "8px" } },
          "轨迹",
          h("span", { className: "sy-tag is-mute" }, phases.length + " 阶段"),
          h("span", { className: "sy-tag is-mute" }, cps.length + " 确认点"),
          fails.length ? h("span", { className: "sy-tag is-err" }, fails.length + " 失败") : null),
        items.length
          ? h("div", null, items)
          : h(SYView.Empty, {
            icon: "证",
            iconSize: 36,
            title: "没有可展示的轨迹事件",
            body: "这份日志里没有 phase_entered / checkpoint_answered / 失败调用记录。"
          }),
        h("div", { className: "sy-sec" },
          h("div", { className: "sy-note" }, "来源：" + orDash(cards.source.path))));
    };

    // ── 知识库全文阅读抽屉 ──
    SYView.kbDrawer = function (vm) {
      var kbDoc = vm.kbDoc;
      if (!kbDoc || !kbDoc.open) return null;
      var body;
      if (kbDoc.phase === "loading") {
        body = h(SYView.Loading, { label: "读取知识库正文…" });
      } else if (kbDoc.phase === "error") {
        body = h(SYView.ErrorBox, {
          message: "正文读取失败：" + (kbDoc.error || "未知错误"),
          onRetry: kbDoc.slug && vm.actions.openKbDoc
            ? function () { vm.actions.openKbDoc(kbDoc.slug); }
            : null
        });
      } else if (kbDoc.data) {
        body = kbDoc.data.markdown || "";
      } else {
        body = "";
      }
      var title = (kbDoc.data && kbDoc.data.title) || kbDoc.slug || "知识库文档";
      return h("div", {
        className: "sy-kb-drawer",
        role: "dialog",
        "aria-modal": "false",
        "aria-labelledby": "sy-kb-doc-title"
      },
        h("div", { className: "sy-kb-drawer-head" },
          h(Icon, { name: "知", size: 16 }),
          h("h2", { id: "sy-kb-doc-title", className: "sy-kb-drawer-title", title: title }, title),
          kbDoc.slug ? h(SYView.Tag, { tone: "mute" }, kbDoc.slug) : null,
          h("button", {
            type: "button",
            className: "sy-btn",
            onClick: function () { if (vm.actions.closeKbDoc) vm.actions.closeKbDoc(); }
          }, "关闭")),
        h("div", { className: "sy-kb-drawer-body" }, body));
    };

    // ── 右检查器 ──
    SYView.inspector = function (vm) {
      var cards = vm.cards.phase === "ok" ? vm.cards.data : null;
      var kb = vm.kb || { phase: "loading", count: 0, items: [] };
      var kbBody;
      if (kb.phase === "ok") {
        kbBody = h("div", null,
          h("div", { className: "sy-insp-row" },
            h("span", { className: "sy-k" }, "文档"),
            h("span", { className: "sy-v" }, SYData.fmtNum(kb.count) + " 篇")),
          (kb.items || []).slice(0, 8).map(function (d, i) {
            var slug = d.slug || "";
            return h("button", {
              type: "button",
              className: "sy-kb-btn",
              key: "kb" + i,
              title: (d.summary ? d.summary + " · " : "") + slug,
              onClick: function () { if (vm.actions.openKbDoc && slug) vm.actions.openKbDoc(slug); }
            }, "· " + (d.title || slug));
          }));
      } else if (kb.phase === "error") {
        kbBody = h("div", { className: "sy-note" }, "知识库路由未就绪（/api/shangyi/kb 由 host-data 提供）。");
      } else {
        kbBody = h("div", { className: "sy-note" }, "读取中…");
      }

      var meta = cards ? h("div", { className: "sy-insp-sec" },
        h("h3", null, "案例"),
        h("div", { className: "sy-insp-row" }, h("span", { className: "sy-k" }, "case_id"), h("span", { className: "sy-v" }, orDash(cards.meta.caseId))),
        h("div", { className: "sy-insp-row" }, h("span", { className: "sy-k" }, "模式"), h("span", { className: "sy-v" }, SYData.modeLabel(cards.meta.mode))),
        h("div", { className: "sy-insp-row" }, h("span", { className: "sy-k" }, "范围"), h("span", { className: "sy-v" }, SYData.scopeLabel(cards.meta.scope))),
        h("div", { className: "sy-insp-row" }, h("span", { className: "sy-k" }, "事件"), h("span", { className: "sy-v" }, SYData.fmtNum(cards.meta.events))),
        h("div", { className: "sy-insp-row" }, h("span", { className: "sy-k" }, "工具调用"), h("span", { className: "sy-v" }, SYData.fmtNum(cards.meta.tools))),
        h("div", { className: "sy-insp-row" }, h("span", { className: "sy-k" }, "token"), h("span", { className: "sy-v" }, SYData.fmtNum(cards.meta.tokens)))) : null;

      var checkpoints = cards ? h("div", { className: "sy-insp-sec" },
        h("h3", null, "确认点"),
        cards.pendingCheckpoint
          ? h("div", { className: "sy-insp-row" },
            h("span", { className: "sy-k" }, "待确认"),
            h(SYView.Tag, { tone: "warn" }, String(cards.pendingCheckpoint)))
          : null,
        (cards.meta.checkpoints || []).length
          ? cards.meta.checkpoints.map(function (cp, i) {
            return h("div", { className: "sy-insp-row", key: "cp" + i }, h("span", { className: "sy-v", style: { textAlign: "left" } }, String(cp)));
          })
          : (!cards.pendingCheckpoint ? h("div", { className: "sy-note" }, "本案例没有已答复的确认点。") : null)) : null;

      var params = cards ? h("div", { className: "sy-insp-sec" },
        h("h3", null, "参数"),
        h("div", { className: "sy-insp-row" },
          h("span", { className: "sy-k" }, "矩阵参数数"),
          h("span", { className: "sy-v" }, cards.matrix && typeof cards.matrix.params === "number" ? String(cards.matrix.params) : "—")),
        h("div", { className: "sy-note" }, "参数终值由引擎求解；矩阵里显示为 ⟨参数⟩ 引用。")) : null;

      var tools = cards ? h("div", { className: "sy-insp-sec" },
        h("h3", null, "工具调用摘要"),
        h("div", { className: "sy-insp-row" }, h("span", { className: "sy-k" }, "总调用"), h("span", { className: "sy-v" }, SYData.fmtNum(cards.meta.tools))),
        h("div", { className: "sy-insp-row" }, h("span", { className: "sy-k" }, "失败（最近 5 条）"),
          h("span", { className: cards.failures.length ? "sy-v sy-tag is-err" : "sy-v" }, String(cards.failures.length))),
        cards.failures.length
          ? h("div", { className: "sy-note" }, "最近失败：" + orDash(cards.failures[cards.failures.length - 1].tool))
          : null) : null;

      return h("aside", { className: "sy-wb-right" + (vm.inspectorOpen ? " is-open" : ""), "aria-label": "检查器" },
        meta,
        checkpoints,
        params,
        tools,
        h("div", { className: "sy-insp-sec" },
          h("h3", null, "知识引用"),
          kbBody),
        cards ? h("div", { className: "sy-insp-sec" },
          h("h3", null, "来源"),
          h("div", { className: "sy-note", style: { wordBreak: "break-all" } }, orDash(cards.source.path)),
          h("div", { className: "sy-insp-row" }, h("span", { className: "sy-k" }, "日志时间"), h("span", { className: "sy-v" }, SYData.fmtTime(cards.source.mtime)))) : null);
    };

    // ── 中栏：状态分发 ──
    SYView.center = function (vm) {
      // 列表还没回来：中栏给骨架，不要提前说「选一个案例」。
      if (vm.cases.phase === "loading" && !vm.cases.items.length) {
        return h("div", { className: "sy-wb-center" }, h("div", { style: { padding: "24px" } },
          h(SYView.Loading, { label: "读取案例列表…" })));
      }
      if (vm.cases.phase === "error" && !vm.cases.items.length) {
        return h("div", { className: "sy-wb-center" }, h("div", { style: { padding: "24px" } },
          h(SYView.ErrorBox, { message: "案例列表读取失败：" + (vm.cases.error || "未知错误"), onRetry: vm.actions.refresh })));
      }
      if (vm.cases.phase === "ok" && !vm.cases.items.length) {
        return h("div", { className: "sy-wb-center" },
          h(SYView.Empty, {
            icon: "档",
            iconSize: 40,
            title: "还没有案例",
            body: "商奕把每次咨询 / 研究 / 费用测算都写成一个 case.jsonl，本页读的是 $SHANGYI_HOME/cases 下的真实日志。现在一条都没有——先去对话面板发起一次商奕任务，回来这里就有数据。",
            actions: [
              h("button", { key: "talk", type: "button", className: "sy-btn is-primary", onClick: vm.actions.openConversation }, "打开对话面板"),
              h("button", { key: "refresh", type: "button", className: "sy-btn", onClick: vm.actions.refresh }, "刷新")
            ]
          }));
      }
      if (!vm.selectedCaseId) {
        return h("div", { className: "sy-wb-center" },
          h(SYView.Empty, {
            icon: "弈",
            iconSize: 36,
            title: "从左侧选一个案例",
            body: "列表里每个案例都是一份真实 case.jsonl；选中后这里显示它的阶段链、支付矩阵、均衡与选择。",
            actions: [h("button", {
              key: "first", type: "button", className: "sy-btn is-primary",
              onClick: function () {
                var first = vm.cases.items[0];
                if (first) vm.actions.selectCase(first.caseId);
              }
            }, "选中最近一个")]
          }));
      }

      var cardsState = vm.cards;
      if (cardsState.phase === "loading" || cardsState.phase === "idle") {
        return h("div", { className: "sy-wb-center" }, h("div", { style: { padding: "24px" } },
          h(SYView.Loading, { label: "读取案例日志…" })));
      }
      if (cardsState.phase === "error") {
        return h("div", { className: "sy-wb-center" }, h("div", { style: { padding: "24px" } },
          h(SYView.ErrorBox, { message: "案例数据读取失败：" + (cardsState.error || "未知错误"), onRetry: vm.actions.retryCards })));
      }

      var cards = cardsState.data;
      var body;
      if (vm.tab === "matrix") body = SYView.matrix(vm);
      else if (vm.tab === "equilibrium") body = SYView.equilibrium(vm);
      else if (vm.tab === "trail") body = SYView.trail(vm);
      else body = SYView.overview(vm);

      return h("div", { className: "sy-wb-center" },
        SYView.caseHead(cards),
        SYView.tabs(vm),
        h("div", { className: "sy-content", role: "tabpanel" }, body));
    };

    /** 工作台根视图（纯函数）。 */
    SYView.renderPanel = function (vm) {
      return h("div", { className: "sy-wb" },
        h("style", null, SY_CSS),
        h("header", { className: "sy-wb-head" },
          h("span", { className: "sy-logo" }, h(Icon, { name: "弈", size: 20 })),
          h("h1", null, "商奕工作台", h("span", { className: "sy-sub" }, "博弈论金融决策 · 只读日志投影")),
          h("span", { className: "sy-spacer" }),
          h("button", { type: "button", className: "sy-btn", onClick: vm.actions.refresh }, "刷新"),
          h("button", {
            type: "button",
            className: "sy-btn sy-insp-toggle",
            onClick: vm.actions.toggleInspector
          }, vm.inspectorOpen ? "收起检查器" : "检查器")),
        h("div", { className: "sy-wb-body" },
          SYView.casesColumn(vm),
          SYView.center(vm),
          SYView.inspector(vm)),
        SYView.kbDrawer(vm));
    };
