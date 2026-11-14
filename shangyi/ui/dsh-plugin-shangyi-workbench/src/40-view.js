    // ──────────────────────────────────────────────────────────────
    // 视图层：纯函数。renderPanel(vm) 不读 hooks、不碰 DOM、不发请求，
    // 因此可以在没有浏览器的地方用假 view model 直接跑（见 scripts/verify.mjs）。
    // ──────────────────────────────────────────────────────────────
    var SYView = {};

    var TAB_DEFS = [
      { id: "overview", label: "总览", icon: "档" },
      { id: "simulation", label: "推演", icon: "弈" },
      { id: "costs", label: "精算", icon: "算" },
      { id: "assumptions", label: "假设", icon: "判" },
      { id: "evidence", label: "证据", icon: "证" },
      { id: "kb", label: "知识库", icon: "知" }
    ];

    function normalizeTabId(tab) {
      if (tab === "matrix" || tab === "equilibrium") return "simulation";
      if (tab === "trail") return "evidence";
      return tab || "overview";
    }

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
      if (status === "waiting") return h(SYView.Tag, { tone: "warn" }, "待确认");
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
          h("span", null, "剧本 ", h("b", null, orDash(SYData.playbookLabel(m.playbook)))),
          h("span", null, "范围 ", h("b", null, SYData.scopeLabel(m.scope))),
          h("span", null, "创建 ", h("b", null, SYData.fmtTime(m.createdAt))),
          h("span", null, "日志 ", h("b", null, SYData.fmtTime(cards.source.mtime)))));
    };

    SYView.tabs = function (vm) {
      var active = normalizeTabId(vm.tab);
      return h("div", { className: "sy-tabs", role: "tablist", "aria-label": "案例视图" },
        TAB_DEFS.map(function (t) {
          return h("button", {
            key: t.id,
            type: "button",
            role: "tab",
            className: "sy-tab",
            "aria-selected": active === t.id ? "true" : "false",
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
        ? h("div", { className: "sy-status", role: "status" },
            h("span", { style: { flex: "1 1 auto" } }, cpForm.message),
            vm.actions && vm.actions.openConversation
              ? h("button", { type: "button", className: "sy-btn is-primary", onClick: vm.actions.openConversation }, "回到对话面板")
              : null)
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

    // ── 总览（白话结论 · 下一步行动 · 产物总览 · 报告导出） ──
    SYView.overview = function (vm) {
      var cards = vm.cards.data;
      var m = cards.meta;
      var chain = SYData.phaseChain(cards);
      var current = chain.length ? chain[chain.length - 1] : null;
      var conc = cards.conclusion || {};
      var rec = conc.recommend;
      var scopeDetail = cards.scopeDetail || {};
      var cond = scopeDetail.conditional;
      var costs = cards.costs || { scenarios: [] };
      var assum = cards.assumptions || { params: [], gaps: [] };

      var conclusionCard = null;
      if (conc.hasExploratoryOnly) {
        conclusionCard = h("div", { className: "sy-cp-card", role: "alert" },
          h("div", { className: "sy-cp-head" },
            h(Icon, { name: "证", size: 16 }),
            h("h3", { className: "sy-cp-title" }, "当前仅有探索性沙箱试算，尚无确定性正式结论")),
          h("div", { className: "sy-note" },
            "日志中仅包含 `sandbox_exec`（TrustLevel::Exploratory）调用。依据铁律 1，沙箱数字已隔离排除，不得作为正式决策结论或报告依据。"));
      } else if (conc.headline || rec || cond || (conc.reportModel && conc.reportModel.sections)) {
        conclusionCard = h("section", { className: "sy-conclusion", "aria-label": "核心结论与行动建议" },
          h("div", { className: "sy-cp-head" },
            h(Icon, { name: "档", size: 16 }),
            h("h3", { className: "sy-cp-title" }, "白话结论与下一步"),
            m.scope ? h(SYView.Tag, { tone: "ok" }, SYData.scopeLabel(m.scope)) : null,
            cond ? h(SYView.Tag, { tone: "warn" }, "条件交付") : null),
          conc.headline ? h("p", { className: "sy-conclusion-hl" }, conc.headline) : null,
          rec && Array.isArray(rec.actions) && rec.actions.length
            ? h("div", null,
                h("strong", { style: { fontSize: "12px" } }, "建议推进动作"),
                h("ol", { className: "sy-act-list" }, rec.actions.map(function (a, i) {
                  return h("li", { key: "act" + i },
                    h("span", null, (a && a.action) || String(a || "")),
                    a && a.rationale ? h("div", { className: "sy-act-why" }, "依据：" + a.rationale) : null);
                })))
            : null,
          rec && Array.isArray(rec.do_not) && rec.do_not.length
            ? h("div", { style: { marginTop: "8px" } },
                h("strong", { style: { fontSize: "12px", color: "var(--dsw-alias-state-error-primary)" } }, "明确不建议（底线避坑）"),
                h("ul", { className: "sy-act-list" }, rec.do_not.map(function (d, i) {
                  return h("li", { key: "dn" + i }, d);
                })))
            : null,
          cond && Array.isArray(cond.decision_conditions) && cond.decision_conditions.length
            ? h("div", { style: { marginTop: "8px" } },
                h("strong", { style: { fontSize: "12px", color: "var(--dsw-alias-state-warn-primary)" } }, "条件交付边界与补证动作"),
                h("ul", { className: "sy-act-list" },
                  cond.decision_conditions.map(function (c, i) { return h("li", { key: "cc" + i }, "前提条件：" + c); })
                    .concat((cond.information_actions || []).map(function (ia, j) { return h("li", { key: "ia" + j }, "待补证据：" + ia); }))))
            : null,
          h("div", { className: "sy-toolbar-row" },
            h("button", {
              type: "button",
              className: "sy-btn is-primary",
              onClick: function () { if (vm.actions && vm.actions.exportReport) vm.actions.exportReport(); }
            }, "导出 Markdown 报告"),
            h("button", {
              type: "button",
              className: "sy-btn",
              onClick: function () { if (vm.actions && vm.actions.printReport) vm.actions.printReport(); }
            }, "打印 / 另存 PDF")));
      }

      var exportFallback = !conclusionCard && conc.hasEngineResult
        ? h("div", { className: "sy-sec", style: { display: "flex", gap: "8px", flexWrap: "wrap" } },
            h("button", {
              type: "button",
              className: "sy-btn is-primary",
              onClick: function () { if (vm.actions && vm.actions.exportReport) vm.actions.exportReport(); }
            }, "导出已产出引擎成果（Markdown）"),
            h("button", {
              type: "button",
              className: "sy-btn",
              onClick: function () { if (vm.actions && vm.actions.printReport) vm.actions.printReport(); }
            }, "打印 / 另存 PDF"))
        : null;

      return h("div", null,
        SYView.checkpointCard(vm),
        conclusionCard || exportFallback,
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
          h("div", { className: "sy-sec-h" }, "六大业务面板覆盖情况"),
          h("div", { className: "sy-kv" },
            h("span", null, "支付矩阵 ", cards.matrix ? h("b", null, "有") : h("b", null, "无")),
            h("span", null, "均衡 ", (cards.solve && cards.solve.equilibria && cards.solve.equilibria.length ? h("b", null, cards.solve.equilibria.length + " 个") : h("b", null, "无"))),
            h("span", null, "选择分布 ", (cards.selection && cards.selection.entries && cards.selection.entries.length ? h("b", null, "有") : h("b", null, "无"))),
            h("span", null, "成本方案 ", h("b", null, costs.scenarios.length ? (costs.scenarios.length + " 个") : "无")),
            h("span", null, "假设参数 ", h("b", null, assum.params.length ? (assum.params.length + " 项") : "无")),
            h("span", null, "确认点 ", (m.checkpoints.length ? h("b", null, m.checkpoints.length + " 个") : h("b", null, "无"))))),
        h("div", { className: "sy-sec" },
          h("div", { className: "sy-sec-h" }, "口径说明"),
          h("div", { className: "sy-note" },
            "本页全部数字来自商奕案例日志（case.jsonl）的只读投影：前端只展示，不重算、不估算、不把缺失项补 0。日志行数 ",
            SYData.fmtNum(m.events),
            cards.source && cards.source.badLines > 0 ? "，坏行 " + SYData.fmtNum(cards.source.badLines) : "",
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
          mx.form ? h("span", { className: "sy-tag is-mute" }, "形式：" + SYData.gameFormLabel(mx.form)) : null,
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
      if (confidence.Calibrated) {
        var c = confidence.Calibrated;
        var nSel = Array.isArray(c.selectors) ? c.selectors.length : 0;
        var loss = typeof c.mean_log_loss === "number" ? ("，平均对数损失 " + c.mean_log_loss.toFixed(3)) : "";
        return "选择权重已校准（样本 " + orDash(c.samples) + "，筛选器 " + nSel + " 个" + loss + "）。";
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
        var payText = null;
        if (Array.isArray(eq.payoffs)) {
          payText = "[" + eq.payoffs.join(", ") + "]";
        } else if (eq.payoffs && typeof eq.payoffs === "object") {
          var pk = Object.keys(eq.payoffs);
          if (pk.length) {
            payText = pk.map(function (k) { return k + ": " + eq.payoffs[k]; }).join(" · ");
          }
        }
        return h("div", { className: "sy-eq", key: eq.id || i },
          h("div", { className: "sy-eq-top" },
            h("span", { className: "sy-eq-id" }, orDash(eq.id)),
            eq.kind ? h("span", { className: "sy-eq-kind" }, SYData.eqKindLabel(eq.kind)) : null,
            payText ? h("span", { className: "sy-eq-pay" }, "支付 " + payText) : null,
            pct === null ? null : h("span", { className: "sy-eq-prob" }, pct + "%")),
          pct === null ? null : h("div", { className: "sy-bar", "aria-hidden": true }, h("i", { style: { width: pct + "%" } })));
      });
      return h("div", null,
        h("div", { className: "sy-sec-h", style: { marginTop: "8px" } },
          "均衡与选择",
          solve.route ? h("span", { className: "sy-tag is-mute" }, "路线 " + SYData.solveRouteLabel(solve.route)) : null,
          solve.completeness ? h("span", { className: "sy-tag is-mute" }, "完整度 " + SYData.completenessLabel(solve.completeness)) : null,
          h("span", { className: "sy-tag is-mute" }, solve.equilibria.length + " 个均衡")),
        rows,
        h("div", { className: "sy-sec" },
          h("div", { className: "sy-note" }, SYView.confidenceText(cards.selection && cards.selection.confidence))));
    };

    // ── 推演（支付矩阵 + 均衡与选择 + 条件后果 + 满意策略发现） ──
    SYView.simulation = function (vm) {
      var cards = vm.cards.data;
      var conc = cards.conclusion || {};
      var hasMatrix = Boolean(cards.matrix);
      var hasEq = Boolean(cards.solve && Array.isArray(cards.solve.equilibria) && cards.solve.equilibria.length);
      var csq = conc.consequence;
      var disc = conc.discovery;

      if (!hasMatrix && !hasEq && !csq && !disc) {
        return SYView.matrixEmpty(vm);
      }

      var csqCard = null;
      if (csq) {
        var reactions = Array.isArray(csq.reactions) ? csq.reactions : [];
        csqCard = h("div", { className: "sy-sec" },
          h("div", { className: "sy-sec-h" }, "条件后果预测（whatif）",
            csq.decision ? h(SYView.Tag, { tone: "ok" }, "动作：" + csq.decision) : null),
          csq.note ? h("div", { className: "sy-note", style: { marginBottom: "8px" } }, csq.note) : null,
          reactions.length
            ? h("div", { className: "sy-matrix-wrap" },
                h("table", { className: "sy-matrix" },
                  h("thead", null, h("tr", null,
                    h("th", { scope: "col" }, "对方角色"),
                    h("th", { scope: "col" }, "最优反应"),
                    h("th", { scope: "col" }, "概率/权重"))),
                  h("tbody", null, reactions.map(function (r, idx) {
                    return h("tr", { key: "rx" + idx },
                      h("td", null, orDash(r.player_id || r.opponent || r.actor)),
                      h("td", null, orDash(r.strategy || r.action || r.response)),
                      h("td", null, typeof r.prob === "number" ? Math.round(r.prob * 100) + "%" : orDash(r.probability)));
                  }))))
            : null);
      }

      var discCard = null;
      if (disc) {
        var rep = disc.report || {};
        var satisfied = Array.isArray(rep.satisfied) ? rep.satisfied : [];
        var relax = Array.isArray(rep.relaxation) ? rep.relaxation : [];
        discCard = h("div", { className: "sy-sec" },
          h("div", { className: "sy-sec-h" }, "满意策略发现",
            h(SYView.Tag, { tone: satisfied.length ? "ok" : "warn" }, "达标 " + satisfied.length + " 项"),
            relax.length ? h(SYView.Tag, { tone: "mute" }, "放宽建议 " + relax.length + " 条") : null),
          disc.note ? h("div", { className: "sy-note", style: { marginBottom: "8px" } }, disc.note) : null,
          satisfied.length
            ? h("ul", { className: "sy-act-list" }, satisfied.map(function (s, idx) {
                return h("li", { key: "ds" + idx }, orDash(s.strategy_id || s.name || s.id || JSON.stringify(s)));
              }))
            : null,
          relax.length
            ? h("ul", { className: "sy-act-list" }, relax.map(function (r, idx) {
                return h("li", { key: "dr" + idx }, "放宽方向：" + orDash(r.suggestion || r.reason || r.dimension || JSON.stringify(r)));
              }))
            : null);
      }

      return h("div", null,
        hasMatrix ? SYView.matrix(vm) : null,
        hasEq
          ? h("div", { className: hasMatrix ? "sy-sec" : "" }, SYView.equilibrium(vm))
          : null,
        csqCard,
        discCard);
    };

    // ── 精算（现金支出 vs 可退押金 vs 期间累计 vs 硬约束；缺失绝不填 0） ──
    SYView.costs = function (vm) {
      var cards = vm.cards.data;
      var costs = cards.costs || { scenarios: [], constraints: [], constraintCheck: null, missingItems: [] };
      var scenarios = costs.scenarios || [];
      var check = costs.constraintCheck;
      var missingItems = costs.missingItems || [];

      if (!scenarios.length && !(costs.constraints && costs.constraints.length) && !check) {
        return h(SYView.Empty, {
          icon: "算",
          iconSize: 36,
          title: "本案例还没有成本精算台账",
          body: "尚未调用 business_cost / constraint_check。前端不会估算或用 0 替代未知费用；请在对话中提供报价或租金条款进行确定性精算。",
          actions: [h("button", { key: "talk", type: "button", className: "sy-btn is-primary", onClick: vm.actions.openConversation }, "去对话做费用精算")]
        });
      }

      var constraintCard = null;
      if (check || (costs.constraints && costs.constraints.length)) {
        var elCount = check && Array.isArray(check.eligible_scenario_ids) ? check.eligible_scenario_ids.length : 0;
        var cmpCount = check && Array.isArray(check.comparison_only_scenario_ids) ? check.comparison_only_scenario_ids.length : 0;
        var unkCount = check && Array.isArray(check.unknown_scenario_ids) ? check.unknown_scenario_ids.length : 0;
        constraintCard = h("div", { className: "sy-cost-card" },
          h("div", { className: "sy-cp-head" },
            h(Icon, { name: "算", size: 16 }),
            h("h3", { className: "sy-cp-title" }, "硬约束核对与资金底线"),
            check && typeof check.constraint_revision === "number"
              ? h(SYView.Tag, { tone: "mute" }, "约束版本 rev." + check.constraint_revision)
              : null,
            check ? h(SYView.Tag, { tone: "ok" }, "通过 " + elCount) : null,
            check && cmpCount ? h(SYView.Tag, { tone: "err" }, "超限对照 " + cmpCount) : null,
            check && unkCount ? h(SYView.Tag, { tone: "warn" }, "未知暂不放行 " + unkCount) : null),
          check && check.note ? h("div", { className: "sy-note" }, check.note) : null);
      }

      var missingBanner = missingItems.length
        ? h("div", { className: "sy-cp-card", role: "alert" },
            h("div", { className: "sy-cp-head" },
              h(Icon, { name: "证", size: 16 }),
              h("h3", { className: "sy-cp-title" }, "共有 " + missingItems.length + " 项金额未取得（严格不补 0）")),
            h("ul", { className: "sy-cp-items" }, missingItems.map(function (mi, idx) {
              return h("li", { key: "mi" + idx },
                h("strong", null, mi.scenario + " · " + mi.item),
                "：",
                h("span", { className: "sy-missing-val" }, "未取得"),
                "（" + mi.reason + "）");
            })))
        : null;

      var scenarioCards = scenarios.map(function (sc, idx) {
        var cur = sc.currency || "CNY";
        var assess = sc.constraint_assessment && sc.constraint_assessment.status;
        var ht = sc.horizon_totals || null;
        var items = Array.isArray(sc.items) ? sc.items : [];
        return h("section", { className: "sy-cost-card", key: sc.scenario_id || sc.scenario || idx },
          h("div", { className: "sy-cp-head" },
            h("h3", { className: "sy-cp-title" }, sc.scenario || sc.scenario_id || ("方案 " + (idx + 1))),
            h(SYView.Tag, { tone: sc.costs_complete && sc.amounts_complete !== false ? "ok" : "warn" },
              sc.costs_complete && sc.amounts_complete !== false ? "清单完整" : "含未取得/未完备项"),
            assess ? h(SYView.Tag, { tone: assess === "eligible" ? "ok" : assess === "ineligible" ? "err" : "warn" },
              assess === "eligible" ? "满足约束" : assess === "ineligible" ? "超约束·仅对照" : "约束核对未知") : null),
          h("div", { className: "sy-stats" },
            h("div", { className: "sy-stat" },
              h("div", { className: "sy-k" }, "期初现金总需求（含押金）"),
              h("div", { className: "sy-v", style: { fontSize: "15px" } }, SYData.fmtCostValue(sc.listed_upfront_cash, cur))),
            h("div", { className: "sy-stat" },
              h("div", { className: "sy-k" }, "一次性现金支出"),
              h("div", { className: "sy-v", style: { fontSize: "15px" } }, SYData.fmtCostValue(sc.listed_upfront_cash_expense, cur))),
            h("div", { className: "sy-stat" },
              h("div", { className: "sy-k" }, "可退押金（占压资金）"),
              h("div", { className: "sy-v", style: { fontSize: "15px" } }, SYData.fmtCostValue(sc.listed_refundable_deposits, cur))),
            h("div", { className: "sy-stat" },
              h("div", { className: "sy-k" }, "月固定现金支出"),
              h("div", { className: "sy-v", style: { fontSize: "15px" } }, SYData.fmtCostValue(sc.listed_monthly_fixed_cost, cur)))),
          ht ? h("div", { className: "sy-kv", style: { marginTop: "12px" } },
            h("span", null, "测算周期 ", h("b", null, ht.months + " 个月")),
            h("span", null, "期间现金费用 ", h("b", null, SYData.fmtCostValue(ht.listed_cash_expense, cur))),
            h("span", null, "期间付款累计（未扣退押） ", h("b", null, SYData.fmtCostValue(ht.listed_cash_outflow_before_deposit_return, cur))),
            typeof sc.cash_after_listed_upfront === "number"
              ? h("span", null, "扣除已知期初后余钱 ", h("b", null, SYData.fmtCostValue(sc.cash_after_listed_upfront, cur)))
              : null)
            : null,
          items.length
            ? h("div", { className: "sy-matrix-wrap", style: { marginTop: "12px" } },
                h("table", { className: "sy-matrix" },
                  h("thead", null, h("tr", null,
                    h("th", { scope: "col" }, "费用项目"),
                    h("th", { scope: "col" }, "性质分类"),
                    h("th", { scope: "col" }, "周期"),
                    h("th", { scope: "col" }, "标准化金额"),
                    h("th", { scope: "col" }, "来源"),
                    h("th", { scope: "col" }, "缺项说明"))),
                  h("tbody", null, items.map(function (it, ii) {
                    var isMissing = it.normalized_amount === null || it.normalized_amount === undefined;
                    return h("tr", { key: "ci" + ii },
                      h("td", null, orDash(it.name)),
                      h("td", null, SYData.costTreatmentLabel(it.treatment)),
                      h("td", null, SYData.periodLabel(it.normalized_period)),
                      h("td", null, isMissing
                        ? h("span", { className: "sy-missing-val" }, "未取得")
                        : SYData.fmtCostValue(it.normalized_amount, cur)),
                      h("td", null, orDash(it.source)),
                      h("td", null, orDash(it.missing_amount_reason)));
                  }))))
            : null,
          sc.note ? h("div", { className: "sy-note", style: { marginTop: "8px" } }, sc.note) : null);
      });

      return h("div", { style: { marginTop: "8px" } },
        constraintCard,
        missingBanner,
        scenarioCards);
    };

    // ── 假设（参与方 · 四类证据来源徽标 · 未确认缺口） ──
    SYView.assumptions = function (vm) {
      var cards = vm.cards.data;
      var assum = cards.assumptions || { players: [], params: [], counts: {}, gaps: [] };
      var players = assum.players || [];
      var params = assum.params || [];
      var counts = assum.counts || {};
      var gaps = assum.gaps || [];

      if (!players.length && !params.length && !gaps.length) {
        return h(SYView.Empty, {
          icon: "判",
          iconSize: 36,
          title: "暂无形式化参数与假设记录",
          body: "本案例尚未调用 gamespec_build 登记参与者与参数证据来源，且无成本未核项缺口。",
          actions: [h("button", { key: "talk", type: "button", className: "sy-btn is-primary", onClick: vm.actions.openConversation }, "回对话完善模型")]
        });
      }

      var confirmedCount = (counts.user_confirmed || 0) + (counts.confirmed_assumption || 0);
      var countTags = h("div", { className: "sy-cp-head", style: { marginTop: "8px" } },
        h(SYView.Tag, { tone: "ok" }, "用户确认 " + confirmedCount),
        h(SYView.Tag, { tone: "ok" }, "公开数据 " + (counts.public_data || 0)),
        h(SYView.Tag, { tone: counts.industry_prior ? "warn" : "mute" }, "行业先验 " + (counts.industry_prior || 0)),
        h(SYView.Tag, { tone: (counts.assumed || 0) + (counts.unknown || 0) > 0 ? "err" : "mute" },
          "未确认假设 " + ((counts.assumed || 0) + (counts.unknown || 0))),
        counts.derived ? h(SYView.Tag, { tone: "mute" }, "引擎派生 " + counts.derived) : null);

      return h("div", { style: { marginTop: "8px" } },
        countTags,
        gaps.length
          ? h("div", { className: "sy-cp-card", role: "alert" },
              h("div", { className: "sy-cp-head" },
                h(Icon, { name: "判", size: 16 }),
                h("h3", { className: "sy-cp-title" }, "待核清假设与证据缺口（铁律 2）")),
              h("ul", { className: "sy-cp-items" }, gaps.map(function (g, i) {
                return h("li", { key: "gp" + i }, g);
              })))
          : null,
        players.length
          ? h("div", { className: "sy-sec" },
              h("div", { className: "sy-sec-h" }, "参与方与价值网角色", h(SYView.Tag, { tone: "mute" }, players.length + " 方")),
              h("div", { className: "sy-matrix-wrap" },
                h("table", { className: "sy-matrix" },
                  h("thead", null, h("tr", null,
                    h("th", { scope: "col" }, "参与方"),
                    h("th", { scope: "col" }, "标识"),
                    h("th", { scope: "col" }, "价值网角色"),
                    h("th", { scope: "col" }, "博弈关系"))),
                  h("tbody", null, players.map(function (pl, pi) {
                    return h("tr", { key: "pl" + pi },
                      h("td", null, orDash(pl.name)),
                      h("td", null, orDash(pl.id)),
                      h("td", null, orDash(pl.vnRole)),
                      h("td", null, orDash(pl.relation)));
                  })))))
          : null,
        params.length
          ? h("div", { className: "sy-sec" },
              h("div", { className: "sy-sec-h" }, "参数证据台账", h(SYView.Tag, { tone: "mute" }, params.length + " 项")),
              h("div", { className: "sy-matrix-wrap" },
                h("table", { className: "sy-matrix" },
                  h("thead", null, h("tr", null,
                    h("th", { scope: "col" }, "参数"),
                    h("th", { scope: "col" }, "值 / 范围"),
                    h("th", { scope: "col" }, "单位"),
                    h("th", { scope: "col" }, "四类证据来源"),
                    h("th", { scope: "col" }, "来源凭据 / 说明"))),
                  h("tbody", null, params.map(function (p, i) {
                    return h("tr", { key: p.key || i },
                      h("td", null, orDash(p.name), p.name && p.key && p.name !== p.key ? h("div", { className: "sy-note" }, p.key) : null),
                      h("td", null, orDash(p.valueText)),
                      h("td", null, orDash(p.unit)),
                      h("td", null, h(SYView.Tag, { tone: SYData.paramSourceTone(p.sourceKind) }, SYData.paramSourceLabel(p.sourceKind))),
                      h("td", null, orDash(p.sourceDetail)));
                  })))))
          : null);
    };

    // ── 证据（人工确认 · POI/调研实证 · 公开检索 · 探索性工具隔离 · 工具/阶段轨迹） ──
    SYView.evidence = function (vm) {
      var cards = vm.cards.data;
      var m = cards.meta;
      var ev = cards.evidence || {};
      var checkpoints = (ev.checkpoints && ev.checkpoints.length)
        ? ev.checkpoints
        : (m.checkpoints || []).map(function (id) {
            return { id: id, status: "answered", answers: ["checkpoint_answered"], actor: null };
          });
      var sources = ev.sources || [];
      var pois = ev.poiSurveys || [];
      var records = ev.researchRecords || [];
      var tools = ev.tools || [];
      var fails = cards.failures || [];
      var phases = m.phases || [];

      var exploratoryBanner = ev.exploratoryCalls > 0
        ? h("div", { className: "sy-cp-card", role: "alert" },
            h("div", { className: "sy-cp-head" },
              h(Icon, { name: "证", size: 16 }),
              h("h3", { className: "sy-cp-title" }, "探索性沙箱隔离（sandbox_exec × " + ev.exploratoryCalls + "）")),
            h("div", { className: "sy-note" },
              "以上沙箱试算属于 TrustLevel::Exploratory，已按铁律 1 彻底隔离，未写入正式支付矩阵、成本、结论或报告数字。"))
        : null;

      var cpSection = h("div", { className: "sy-sec" },
        h("div", { className: "sy-sec-h" }, "人类决策关口审计记录",
          h(SYView.Tag, { tone: "mute" }, checkpoints.length + " 次")),
        checkpoints.length
          ? checkpoints.map(function (cp, i) {
              var stTone = cp.status === "answered" ? "ok" : cp.status === "pending" ? "warn" : "mute";
              var stLabel = cp.status === "answered" ? "已核准/答复" : cp.status === "pending" ? "待真人确认" : "已被替代";
              return h("div", { className: "sy-eq", key: "ecp" + i },
                h("div", { className: "sy-eq-top" },
                  h("span", { className: "sy-eq-id" }, SYData.checkpointLabel(cp.id)),
                  h(SYView.Tag, { tone: stTone }, stLabel),
                  cp.actor ? h(SYView.Tag, { tone: "mute" }, "actor=" + cp.actor) : null,
                  cp.answeredAt || cp.openedAt ? h("span", { className: "sy-eq-prob" }, SYData.fmtTime(cp.answeredAt || cp.openedAt)) : null),
                cp.answers && cp.answers.length
                  ? h("div", { className: "sy-note", style: { marginTop: "6px" } }, "答复：" + cp.answers.join(" | "))
                  : null);
            })
          : h("div", { className: "sy-note" }, "暂无人类关口记录。"));

      var srcSection = sources.length
        ? h("div", { className: "sy-sec" },
            h("div", { className: "sy-sec-h" }, "公开检索与抓取来源", h(SYView.Tag, { tone: "mute" }, sources.length + " 条")),
            sources.map(function (s, i) {
              var href = SYData.safeHref(s.url);
              return h("div", { className: "sy-trail-item", key: "src" + i },
                h("div", { className: "sy-trail-when" }, orDash(s.tool)),
                h("div", { className: "sy-trail-body" },
                  h("div", { className: "sy-trail-tool" },
                    href
                      ? h("a", { href: href, target: "_blank", rel: "noopener noreferrer" }, s.title || s.url)
                      : orDash(s.title || s.url)),
                  s.snippet ? h("div", { className: "sy-trail-detail" }, s.snippet) : null,
                  s.title && s.url ? h("div", { className: "sy-note" }, s.url) : null));
            }))
        : null;

      var fieldSection = (pois.length || records.length)
        ? h("div", { className: "sy-sec" },
            h("div", { className: "sy-sec-h" }, "商圈 POI 与门店实地凭据",
              h(SYView.Tag, { tone: "mute" }, "POI " + pois.length + " · 凭据 " + records.length)),
            pois.map(function (p, i) {
              return h("div", { className: "sy-trail-item", key: "poi" + i },
                h("div", { className: "sy-trail-when" }, "POI 计数"),
                h("div", { className: "sy-trail-body" },
                  h("div", { className: "sy-trail-tool" }, orDash(p.address || p.query || "周边统计")),
                  h("div", { className: "sy-trail-detail" }, JSON.stringify(p))));
            }),
            records.map(function (r, i) {
              return h("div", { className: "sy-trail-item", key: "rec" + i },
                h("div", { className: "sy-trail-when" }, "门店凭据"),
                h("div", { className: "sy-trail-body" },
                  h("div", { className: "sy-trail-detail" }, JSON.stringify(r))));
            }))
        : null;

      var trailItems = [];
      for (var i = 0; i < phases.length; i++) {
        trailItems.push(h("div", { className: "sy-trail-item", key: "p" + i },
          h("div", { className: "sy-trail-when" }, "阶段 " + (i + 1)),
          h("div", { className: "sy-trail-body" },
            h("div", { className: "sy-trail-tool" }, SYData.phaseLabel(phases[i])),
            h("div", { className: "sy-trail-detail" }, phases[i]))));
      }
      for (var t = tools.length - 1; t >= 0 && trailItems.length < 40; t--) {
        var tl = tools[t] || {};
        trailItems.push(h("div", { className: "sy-trail-item", key: "tl" + t },
          h("div", { className: "sy-trail-when" }, SYData.toolStatusLabel(tl.status)),
          h("div", { className: "sy-trail-body" },
            h("div", { className: "sy-trail-tool" },
              orDash(tl.name),
              tl.exploratory ? h(SYView.Tag, { tone: "warn", }, "Exploratory 已隔离") : null),
            tl.detail ? h("div", { className: "sy-trail-detail" }, tl.detail) : null)));
      }
      if (!tools.length) {
        for (var k = 0; k < fails.length; k++) {
          var f = fails[k] || {};
          trailItems.push(h("div", { className: "sy-trail-item", key: "f" + k },
            h("div", { className: "sy-trail-when" }, "失败"),
            h("div", { className: "sy-trail-body" },
              h("div", { className: "sy-trail-tool" }, orDash(f.tool)),
              h("div", { className: "sy-trail-detail" }, SYData.toolStatusLabel(f.status) + " · " + orDash(f.detail)))));
        }
      }

      return h("div", null,
        exploratoryBanner,
        cpSection,
        srcSection,
        fieldSection,
        h("div", { className: "sy-sec" },
          h("div", { className: "sy-sec-h" },
            "阶段与工具审计轨迹",
            h("span", { className: "sy-tag is-mute" }, phases.length + " 阶段"),
            h("span", { className: "sy-tag is-mute" }, tools.length + " 调用"),
            fails.length ? h("span", { className: "sy-tag is-err" }, fails.length + " 失败") : null),
          trailItems.length
            ? h("div", null, trailItems)
            : h("div", { className: "sy-note" }, "暂无阶段或工具审计事件。"),
          h("div", { className: "sy-sec" },
            h("div", { className: "sy-note" }, "日志源文件：" + orDash(cards.source.path)))));
    };

    SYView.trail = SYView.evidence;

    // ── 安全纯 React Markdown 渲染（零 dangerouslySetInnerHTML，剥离 YAML front-matter，支持 [[wiki-link]]） ──
    SYView.renderInline = function (text, keyPrefix, onOpenWiki) {
      var tokens = SYData.parseInlineTokens(text);
      return tokens.map(function (tok, idx) {
        var key = keyPrefix + "-" + idx;
        if (tok.type === "bold") return h("strong", { key: key }, tok.text);
        if (tok.type === "code") return h("code", { key: key }, tok.text);
        if (tok.type === "link") {
          return h("a", { key: key, href: tok.href, target: "_blank", rel: "noopener noreferrer" }, tok.text);
        }
        if (tok.type === "wiki") {
          return h("button", {
            key: key,
            type: "button",
            className: "sy-md-wiki",
            title: "跳转到知识条目：" + tok.target,
            onClick: function () { if (onOpenWiki) onOpenWiki(tok.target); }
          }, tok.label);
        }
        return tok.text;
      });
    };

    SYView.Markdown = function (props) {
      var parsed = SYData.parseFrontMatterAndBody(props.markdown || "");
      var meta = parsed.meta || {};
      var blocks = SYData.parseMarkdownBlocks(parsed.body);
      var chips = [];
      if (meta.kind) chips.push(h(SYView.Tag, { key: "mk", tone: "ok" }, SYData.KB_KIND_LABELS[meta.kind] || meta.kind));
      if (meta.id) chips.push(h(SYView.Tag, { key: "mi", tone: "mute" }, "ID " + meta.id));
      if (meta.tags) chips.push(h(SYView.Tag, { key: "mt", tone: "mute" }, meta.tags));

      var rendered = blocks.map(function (b, idx) {
        var kp = "mb" + idx;
        if (b.type === "heading") {
          var tag = b.level === 1 ? "h1" : b.level === 2 ? "h2" : b.level === 3 ? "h3" : "h4";
          return h(tag, { key: kp }, SYView.renderInline(b.text, kp, props.onOpenWiki));
        }
        if (b.type === "code") {
          return h("pre", { key: kp }, h("code", null, b.text));
        }
        if (b.type === "hr") return h("hr", { key: kp });
        if (b.type === "quote") {
          return h("blockquote", { key: kp }, SYView.renderInline(b.text, kp, props.onOpenWiki));
        }
        if (b.type === "ul") {
          return h("ul", { key: kp }, b.items.map(function (it, li) {
            return h("li", { key: kp + "u" + li }, SYView.renderInline(it, kp + "u" + li, props.onOpenWiki));
          }));
        }
        if (b.type === "ol") {
          return h("ol", { key: kp }, b.items.map(function (it, li) {
            return h("li", { key: kp + "o" + li }, SYView.renderInline(it, kp + "o" + li, props.onOpenWiki));
          }));
        }
        if (b.type === "table") {
          return h("div", { className: "sy-matrix-wrap", style: { margin: "10px 0" }, key: kp },
            h("table", { className: "sy-matrix" },
              h("thead", null, h("tr", null, b.headers.map(function (hd, hi) {
                return h("th", { key: kp + "th" + hi, scope: "col" }, SYView.renderInline(hd, kp + "th" + hi, props.onOpenWiki));
              }))),
              h("tbody", null, b.rows.map(function (row, ri) {
                return h("tr", { key: kp + "tr" + ri }, row.map(function (cell, ci) {
                  return h("td", { key: kp + "td" + ri + "-" + ci }, SYView.renderInline(cell, kp + "td" + ri + "-" + ci, props.onOpenWiki));
                }));
              }))));
        }
        return h("p", { key: kp }, SYView.renderInline(b.text, kp, props.onOpenWiki));
      });

      return h("div", { className: "sy-md" },
        chips.length ? h("div", { className: "sy-cp-head", style: { marginBottom: "8px" } }, chips) : null,
        meta.summary ? h("blockquote", { key: "msum" }, meta.summary) : null,
        rendered);
    };

    // ── 知识库页签（搜索 + 分类浏览 + 点击打开阅读抽屉） ──
    SYView.kbTab = function (vm) {
      var kb = vm.kb || { phase: "loading", count: 0, items: [] };
      if (kb.phase === "loading") return h(SYView.Loading, { label: "读取本地知识库…" });
      if (kb.phase === "error") {
        return h(SYView.ErrorBox, { message: "知识库尚未就绪，请确认 SHANGYI_ROOT 或 host-data 服务。", onRetry: vm.actions.refresh });
      }
      var query = (vm.kbQuery || "").trim().toLowerCase();
      var kind = vm.kbKind || "all";
      var docs = (kb.items || []).filter(function (d) {
        if (kind !== "all" && (d.group || "other") !== kind) return false;
        if (!query) return true;
        var hay = [d.title, d.slug, d.summary, d.id].join(" ").toLowerCase();
        return hay.indexOf(query) >= 0;
      });

      var kinds = [
        { id: "all", label: "全部（" + (kb.items || []).length + "）" },
        { id: "theory", label: "核心理论" },
        { id: "method", label: "研究方法" },
        { id: "concept", label: "决策概念" }
      ];

      return h("div", { style: { marginTop: "8px" } },
        h("div", { className: "sy-kb-toolbar" },
          h("input", {
            type: "search",
            className: "sy-kb-search",
            value: vm.kbQuery || "",
            placeholder: "检索博弈理论、精算规则或研究方法…",
            "aria-label": "检索知识库",
            onChange: function (e) { if (vm.actions.setKbQuery) vm.actions.setKbQuery(e.target.value); }
          }),
          kinds.map(function (k) {
            var active = kind === k.id;
            return h("button", {
              key: k.id,
              type: "button",
              className: active ? "sy-btn is-primary" : "sy-btn",
              onClick: function () { if (vm.actions.setKbKind) vm.actions.setKbKind(k.id); }
            }, k.label);
          })),
        docs.length
          ? h("div", { className: "sy-kb-grid" }, docs.map(function (d, i) {
              var slug = d.slug || "";
              return h("button", {
                key: slug || i,
                type: "button",
                className: "sy-kb-card",
                onClick: function () { if (vm.actions.openKbDoc && slug) vm.actions.openKbDoc(slug); }
              },
                h("div", { className: "sy-cp-head", style: { marginBottom: "2px" } },
                  h("span", { className: "sy-ex-title" }, d.title || slug),
                  d.group ? h(SYView.Tag, { tone: "mute" }, SYData.KB_KIND_LABELS[d.group] || d.group) : null,
                  d.userAdded ? h(SYView.Tag, { tone: "ok" }, "自建条目") : null),
                d.summary ? h("div", { className: "sy-ex-hint" }, d.summary) : null,
                h("div", { className: "sy-note" }, slug));
            }))
          : h(SYView.Empty, {
              icon: "知",
              iconSize: 36,
              title: "没有匹配的知识库条目",
              body: "清空搜索关键词或切换分类试试；也可以用命令 `shangyi kb add` 导入自定义 Markdown 经验条目。"
            }));
    };

    // ── 健康告警条（缺二进制 / MCP 未连接时强提示，防止静默退化成裸 LLM 对话） ──
    SYView.healthBanner = function (vm) {
      var hs = vm.health;
      if (!hs || hs.phase !== "ok" || !hs.data || hs.data.ok !== false) return null;
      var problems = Array.isArray(hs.data.problems) ? hs.data.problems : [];
      if (!problems.length) return null;
      return h("div", { className: "sy-health-alert", role: "alert" },
        problems.map(function (p, idx) {
          var cmds = Array.isArray(p.fixCommands) ? p.fixCommands : [];
          return h("div", { key: p.code || idx, style: idx > 0 ? { marginTop: "8px", paddingTop: "8px", borderTop: "1px solid var(--dsw-alias-border-l1)" } : null },
            h("div", { className: "sy-health-head" },
              h(Icon, { name: "证", size: 16 }),
              h("span", { className: "sy-health-title" }, p.title || "商奕确定性内核未就绪"),
              h(SYView.Tag, { tone: p.severity === "blocking" ? "err" : "warn" }, p.severity === "blocking" ? "阻断风险" : "警告"),
              idx === 0 && vm.actions && vm.actions.refreshHealth
                ? h("button", { type: "button", className: "sy-btn", style: { marginLeft: "auto" }, onClick: vm.actions.refreshHealth }, "重新检测")
                : null),
            p.detail ? h("div", { className: "sy-note", style: { color: "var(--dsw-alias-label-primary)" } }, p.detail) : null,
            cmds.length
              ? h("div", { style: { display: "flex", gap: "8px", flexWrap: "wrap", marginTop: "4px" } },
                  cmds.map(function (cmd, ci) {
                    return h("code", { className: "sy-health-cmd", key: "hc" + ci }, cmd);
                  }))
              : null);
        }));
    };

    // ── 新手引导（输入什么 + 3 步怎么走 + 4 个可点击示例提问） ──
    SYView.onboarding = function (vm, opts) {
      var canClose = opts && opts.canClose;
      var examples = SYData.ONBOARDING_EXAMPLES || [];
      return h("div", { className: "sy-guide" },
        h("div", { className: "sy-guide-hero" },
          h("div", { className: "sy-cp-head" },
            h(Icon, { name: "弈", size: 20 }),
            h("h2", null, "欢迎使用商奕 · 商业与金融博弈决策工作台"),
            canClose && vm.actions && vm.actions.toggleGuide
              ? h("button", { type: "button", className: "sy-btn", style: { marginLeft: "auto" }, onClick: vm.actions.toggleGuide }, "返回当前案例")
              : null),
          h("p", null,
            "不用背博弈术语。你只需要在对话面板用日常语言描述商业处境、报价清单或担忧的问题：商奕先检索公开事实，把金额交给 Rust 确定性内核精算（缺项标“未取得”，绝不估算补 0），最后再由你亲手核准关键假设并生成均衡建议与可追溯报告。")),
        h("div", { className: "sy-steps" },
          h("div", { className: "sy-step-card" },
            h("div", { className: "sy-cp-head" }, h("span", { className: "sy-step-num" }, "1"), h("strong", null, "输入处境与底线")),
            h("div", { className: "sy-ex-hint" }, "说明对手/客户关系、可用现金上限、已有报价或房租合同，未知项直接说明即可。")),
          h("div", { className: "sy-step-card" },
            h("div", { className: "sy-cp-head" }, h("span", { className: "sy-step-num" }, "2"), h("strong", null, "核对证据与关口")),
            h("div", { className: "sy-ex-hint" }, "关键假设与建模前必须在关口（cp1/cp2/cp3）由你亲自核准，未确认假设绝不静默进求解器。")),
          h("div", { className: "sy-step-card" },
            h("div", { className: "sy-cp-head" }, h("span", { className: "sy-step-num" }, "3"), h("strong", null, "复盘六大面板与导出")),
            h("div", { className: "sy-ex-hint" }, "总览看白话行动，推演/精算/假设/证据核实数字与依据，并一键导出 Markdown 或 PDF 报告。"))),
        h("div", { className: "sy-sec-h" }, "点击以下任意行业示例，一键复制提问并前往对话面板"),
        h("div", { className: "sy-examples" }, examples.map(function (ex) {
          return h("button", {
            key: ex.id,
            type: "button",
            className: "sy-ex-card",
            onClick: function () {
              if (vm.actions && vm.actions.useExamplePrompt) vm.actions.useExamplePrompt(ex.prompt);
              else if (vm.actions && vm.actions.openConversation) vm.actions.openConversation();
            }
          },
            h("div", { className: "sy-cp-head", style: { marginBottom: "0" } },
              h("span", { className: "sy-ex-title" }, ex.title),
              h(SYView.Tag, { tone: "ok" }, ex.tag)),
            h("div", { className: "sy-ex-hint" }, ex.hint),
            h("div", { className: "sy-note" }, "点击复制示例并去对话 →"));
        })),
        h("div", { className: "sy-toolbar-row", style: { marginTop: "16px" } },
          h("button", { type: "button", className: "sy-btn is-primary", onClick: vm.actions.openConversation }, "打开对话面板自己输入"),
          h("button", { type: "button", className: "sy-btn", onClick: function () { vm.actions.setTab("kb"); } }, "先逛逛 35 篇博弈知识库"),
          h("button", { type: "button", className: "sy-btn", onClick: vm.actions.refresh }, "刷新案例列表")));
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
        body = h(SYView.Markdown, {
          markdown: kbDoc.data.markdown || "",
          onOpenWiki: function (target) {
            if (vm.actions && vm.actions.openKbDoc) vm.actions.openKbDoc(target);
          }
        });
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
            h(SYView.Tag, { tone: "warn" }, SYData.checkpointLabel(cards.pendingCheckpoint)))
          : null,
        (cards.meta.checkpoints || []).length
          ? cards.meta.checkpoints.map(function (cp, i) {
            return h("div", { className: "sy-insp-row", key: "cp" + i }, h("span", { className: "sy-v", style: { textAlign: "left" } }, SYData.checkpointLabel(cp)));
          })
          : (!cards.pendingCheckpoint ? h("div", { className: "sy-note" }, "本案例没有已答复的确认点。") : null)) : null;

      var params = cards ? h("div", { className: "sy-insp-sec" },
        h("h3", null, "参数与证据"),
        h("div", { className: "sy-insp-row" },
          h("span", { className: "sy-k" }, "矩阵参数数"),
          h("span", { className: "sy-v" }, cards.matrix && typeof cards.matrix.params === "number" ? String(cards.matrix.params) : "—")),
        h("div", { className: "sy-insp-row" },
          h("span", { className: "sy-k" }, "证据缺口"),
          h("span", { className: "sy-v" }, String(cards.assumptions && cards.assumptions.gaps ? cards.assumptions.gaps.length : 0))),
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
      var banner = SYView.healthBanner(vm);
      var toast = vm.toastMsg
        ? h("div", { className: "sy-status", role: "status", style: { margin: "12px 24px 0" } }, vm.toastMsg)
        : null;
      var activeTab = normalizeTabId(vm.tab);

      if (vm.guideOpen) {
        return h("div", { className: "sy-wb-center" },
          banner,
          toast,
          h("div", { className: "sy-content" }, SYView.onboarding(vm, { canClose: vm.cases.items.length > 0 })));
      }

      if (activeTab === "kb" && !vm.selectedCaseId) {
        return h("div", { className: "sy-wb-center" },
          banner,
          toast,
          h("div", { className: "sy-casehead", style: { display: "flex", alignItems: "center", gap: "8px" } },
            h("h2", { className: "sy-casename", style: { margin: 0, flex: "1 1 auto" } }, "商奕知识库与决策方法论"),
            h("button", { type: "button", className: "sy-btn", onClick: function () { vm.actions.setTab("overview"); } }, "返回引导")),
          h("div", { className: "sy-content" }, SYView.kbTab(vm)));
      }

      // 列表还没回来：中栏给骨架，不要提前说「选一个案例」。
      if (vm.cases.phase === "loading" && !vm.cases.items.length) {
        return h("div", { className: "sy-wb-center" },
          banner,
          toast,
          h("div", { style: { padding: "24px" } }, h(SYView.Loading, { label: "读取案例列表…" })));
      }
      if (vm.cases.phase === "error" && !vm.cases.items.length) {
        return h("div", { className: "sy-wb-center" },
          banner,
          toast,
          h("div", { style: { padding: "24px" } },
            h(SYView.ErrorBox, { message: "案例列表读取失败：" + (vm.cases.error || "未知错误"), onRetry: vm.actions.refresh })));
      }
      if (vm.cases.phase === "ok" && !vm.cases.items.length) {
        return h("div", { className: "sy-wb-center" },
          banner,
          toast,
          h("div", { className: "sy-content" }, SYView.onboarding(vm, { canClose: false })));
      }
      if (!vm.selectedCaseId) {
        return h("div", { className: "sy-wb-center" },
          banner,
          toast,
          h(SYView.Empty, {
            icon: "弈",
            iconSize: 36,
            title: "从左侧选一个案例",
            body: "列表里每个案例都是一份真实 case.jsonl；选中后可查看六大业务模块：总览、推演、精算、假设、证据与知识库。",
            actions: [
              h("button", {
                key: "first", type: "button", className: "sy-btn is-primary",
                onClick: function () {
                  var first = vm.cases.items[0];
                  if (first) vm.actions.selectCase(first.caseId);
                }
              }, "选中最近一个"),
              h("button", { key: "kb", type: "button", className: "sy-btn", onClick: function () { vm.actions.setTab("kb"); } }, "浏览知识库")
            ]
          }));
      }

      var cardsState = vm.cards;
      if (cardsState.phase === "loading" || cardsState.phase === "idle") {
        return h("div", { className: "sy-wb-center" },
          banner,
          toast,
          h("div", { style: { padding: "24px" } }, h(SYView.Loading, { label: "读取案例日志…" })));
      }
      if (cardsState.phase === "error") {
        return h("div", { className: "sy-wb-center" },
          banner,
          toast,
          h("div", { style: { padding: "24px" } },
            h(SYView.ErrorBox, { message: "案例数据读取失败：" + (cardsState.error || "未知错误"), onRetry: vm.actions.retryCards })));
      }

      var cards = cardsState.data;
      var body;
      if (activeTab === "simulation") body = SYView.simulation(vm);
      else if (activeTab === "costs") body = SYView.costs(vm);
      else if (activeTab === "assumptions") body = SYView.assumptions(vm);
      else if (activeTab === "evidence") body = SYView.evidence(vm);
      else if (activeTab === "kb") body = SYView.kbTab(vm);
      else body = SYView.overview(vm);

      return h("div", { className: "sy-wb-center" },
        banner,
        toast,
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
          vm.actions && vm.actions.toggleGuide
            ? h("button", {
                type: "button",
                className: vm.guideOpen ? "sy-btn is-primary" : "sy-btn",
                onClick: vm.actions.toggleGuide
              }, vm.guideOpen ? "退出引导" : "新手引导")
            : null,
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
