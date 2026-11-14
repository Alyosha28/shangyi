    // ──────────────────────────────────────────────────────────────
    // 面板组件：只做三件事——取数据、管选中、把 view model 交给 SYView。
    // 所有渲染逻辑在 SYView（纯函数），这里只保留 hook 与副作用。
    // ──────────────────────────────────────────────────────────────
    function createWorkbenchPanel(ctx) {
      return function WorkbenchPanel() {
        var stateRef = React.useState({
          cases: { phase: "loading", items: [], error: null },
          query: "",
          selectedCaseId: null,
          cards: { phase: "idle", data: null, error: null },
          tab: "overview",
          inspectorOpen: false,
          guideOpen: false,
          kb: { phase: "loading", count: 0, items: [] },
          kbQuery: "",
          kbKind: "all",
          health: { phase: "loading", data: null, error: null },
          toastMsg: null,
          cpForm: { note: "", phase: "idle", message: null, error: null },
          kbDoc: { open: false, slug: null, phase: "idle", data: null, error: null }
        });
        var state = stateRef[0];
        var setState = stateRef[1];

        var loadCards = React.useCallback(function (caseId) {
          if (!caseId) return;
          setState(function (prev) {
            return Object.assign({}, prev, {
              cards: { phase: "loading", data: null, error: null }
            });
          });
          SYData.getJson(SYData.cardsUrl(caseId))
            .then(function (json) {
              var data = SYData.normalizeCards(json);
              setState(function (prev) {
                return Object.assign({}, prev, {
                  cards: { phase: "ok", data: data, error: null }
                });
              });
            })
            .catch(function (e) {
              setState(function (prev) {
                return Object.assign({}, prev, {
                  cards: { phase: "error", data: null, error: String((e && e.message) || e) }
                });
              });
            });
        }, []);

        var loadCases = React.useCallback(function (opts) {
          var keepSelection = opts && opts.keepSelection;
          setState(function (prev) {
            return Object.assign({}, prev, {
              cases: { phase: "loading", items: prev.cases.items, error: null }
            });
          });
          SYData.getJson(SYData.CASES_URL)
            .then(function (json) {
              var items = SYData.normalizeCases(json);
              setState(function (prev) {
                var selected = keepSelection ? prev.selectedCaseId : null;
                var known = false;
                for (var i = 0; i < items.length; i++) {
                  if (items[i].caseId === selected) { known = true; break; }
                }
                if (!selected || !known) {
                  // 自动选中：优先「有博弈结构」的案例，否则最近一个（列表已按 mtime 倒序）。
                  var pick = null;
                  for (var j = 0; j < items.length; j++) {
                    if (items[j].hasMatrix || items[j].hasSolve) { pick = items[j]; break; }
                  }
                  selected = pick ? pick.caseId : (items.length ? items[0].caseId : null);
                }
                return Object.assign({}, prev, {
                  cases: { phase: "ok", items: items, error: null },
                  selectedCaseId: selected
                });
              });
            })
            .catch(function (e) {
              setState(function (prev) {
                return Object.assign({}, prev, {
                  cases: { phase: "error", items: [], error: String((e && e.message) || e) },
                  selectedCaseId: null,
                  cards: { phase: "idle", data: null, error: null }
                });
              });
            });
        }, []);

        var loadKb = React.useCallback(function () {
          SYData.getJson(SYData.KB_URL)
            .then(function (json) {
              var docs = Array.isArray(json.docs) ? json.docs : [];
              setState(function (prev) {
                return Object.assign({}, prev, { kb: { phase: "ok", count: docs.length, items: docs } });
              });
            })
            .catch(function () {
              // 知识库路由由 host-data 提供，未就绪不算错误：只降级显示。
              setState(function (prev) {
                return Object.assign({}, prev, { kb: { phase: "error", count: 0, items: [] } });
              });
            });
        }, []);

        var loadHealth = React.useCallback(function () {
          SYData.getHealth()
            .then(function (json) {
              setState(function (prev) {
                return Object.assign({}, prev, { health: { phase: "ok", data: json, error: null } });
              });
            })
            .catch(function (e) {
              setState(function (prev) {
                return Object.assign({}, prev, {
                  health: { phase: "error", data: null, error: String((e && e.message) || e) }
                });
              });
            });
        }, []);

        // 首次进入：抓案例列表 + 知识库目录 + 内核健康状态（一次）。
        React.useEffect(function () {
          loadCases();
          loadKb();
          loadHealth();
        }, [loadCases, loadKb, loadHealth]);

        // 选中变化：抓该案例的 cards。
        React.useEffect(function () {
          if (state.selectedCaseId) loadCards(state.selectedCaseId);
        }, [state.selectedCaseId, loadCards]);

        var actions = {
          setQuery: function (v) {
            setState(function (prev) { return Object.assign({}, prev, { query: String(v || "") }); });
          },
          selectCase: function (caseId) {
            setState(function (prev) {
              if (prev.selectedCaseId === caseId && !prev.guideOpen) return prev;
              return Object.assign({}, prev, {
                selectedCaseId: caseId,
                guideOpen: false,
                cards: { phase: "loading", data: null, error: null },
                cpForm: { note: "", phase: "idle", message: null, error: null }
              });
            });
          },
          setCheckpointNote: function (note) {
            setState(function (prev) {
              return Object.assign({}, prev, {
                cpForm: Object.assign({}, prev.cpForm, { note: String(note || "") })
              });
            });
          },
          submitCheckpoint: function (decision) {
            var cards = state.cards && state.cards.data;
            var cp = cards && cards.pendingCheckpointDetail;
            var caseId = (cards && cards.caseId) || state.selectedCaseId;
            if (!caseId || !cp || state.cpForm.phase === "submitting") return;
            var note = (state.cpForm && state.cpForm.note) || "";
            setState(function (prev) {
              return Object.assign({}, prev, {
                cpForm: Object.assign({}, prev.cpForm, { phase: "submitting", message: null, error: null })
              });
            });
            SYData.postCheckpoint({
              caseId: caseId,
              checkpointId: cp.id,
              decision: decision,
              note: note
            })
              .then(function (res) {
                var verb = decision === "approve" ? "已核准通过" : "已驳回并要求修订";
                setState(function (prev) {
                  return Object.assign({}, prev, {
                    cpForm: {
                      note: "",
                      phase: "done",
                      message: verb + "关口 " + (res.checkpointId || cp.id) + "（actor=user），可在对话面板继续推进。",
                      error: null
                    }
                  });
                });
                loadCases({ keepSelection: true });
                loadCards(caseId);
              })
              .catch(function (e) {
                setState(function (prev) {
                  return Object.assign({}, prev, {
                    cpForm: Object.assign({}, prev.cpForm, {
                      phase: "error",
                      error: String((e && e.message) || e)
                    })
                  });
                });
              });
          },
          openKbDoc: function (target) {
            if (!target) return;
            var slug = SYData.resolveKbSlug(target, state.kb && state.kb.items);
            if (!slug) return;
            setState(function (prev) {
              return Object.assign({}, prev, {
                kbDoc: { open: true, slug: slug, phase: "loading", data: null, error: null }
              });
            });
            SYData.getJson(SYData.kbDocUrl(slug))
              .then(function (json) {
                setState(function (prev) {
                  return Object.assign({}, prev, {
                    kbDoc: { open: true, slug: slug, phase: "ok", data: json, error: null }
                  });
                });
              })
              .catch(function (e) {
                setState(function (prev) {
                  return Object.assign({}, prev, {
                    kbDoc: { open: true, slug: slug, phase: "error", data: null, error: String((e && e.message) || e) }
                  });
                });
              });
          },
          closeKbDoc: function () {
            setState(function (prev) {
              return Object.assign({}, prev, {
                kbDoc: { open: false, slug: null, phase: "idle", data: null, error: null }
              });
            });
          },
          setTab: function (tab) {
            setState(function (prev) { return Object.assign({}, prev, { tab: tab, guideOpen: false }); });
          },
          toggleGuide: function () {
            setState(function (prev) { return Object.assign({}, prev, { guideOpen: !prev.guideOpen }); });
          },
          setKbQuery: function (q) {
            setState(function (prev) { return Object.assign({}, prev, { kbQuery: String(q || "") }); });
          },
          setKbKind: function (kind) {
            setState(function (prev) { return Object.assign({}, prev, { kbKind: String(kind || "all") }); });
          },
          toggleInspector: function () {
            setState(function (prev) { return Object.assign({}, prev, { inspectorOpen: !prev.inspectorOpen }); });
          },
          retryCards: function () {
            loadCards(state.selectedCaseId);
          },
          refreshHealth: function () {
            loadHealth();
          },
          refresh: function () {
            loadCases({ keepSelection: true });
            loadKb();
            loadHealth();
            if (state.selectedCaseId) loadCards(state.selectedCaseId);
          },
          openConversation: function () {
            try {
              if (ctx && ctx.layout && typeof ctx.layout.selectPanel === "function") {
                ctx.layout.selectPanel("conversation"); // WORKBENCH-CONTRACT: main 对话面板 key
              }
            } catch (e) {
              // 布局服务不可用时不阻断：用户仍可用侧栏「新会话」切回。
            }
          },
          useExamplePrompt: function (prompt) {
            var text = String(prompt || "").trim();
            if (!text) return;
            var copied = false;
            try {
              if (typeof navigator !== "undefined" && navigator.clipboard && typeof navigator.clipboard.writeText === "function") {
                navigator.clipboard.writeText(text).catch(function () {});
                copied = true;
              } else if (typeof document !== "undefined" && typeof document.createElement === "function") {
                var ta = document.createElement("textarea");
                ta.value = text;
                ta.setAttribute("readonly", "");
                ta.style.position = "fixed";
                ta.style.opacity = "0";
                document.body.appendChild(ta);
                ta.select();
                try { copied = document.execCommand("copy"); } catch (e) { copied = false; }
                document.body.removeChild(ta);
              }
            } catch (e) {
              copied = false;
            }
            setState(function (prev) {
              return Object.assign({}, prev, {
                toastMsg: (copied ? "已复制示例提问到剪贴板，去对话面板粘贴发送即可：" : "已切换至对话面板，请参考示例输入：") +
                  text.slice(0, 26) + (text.length > 26 ? "…" : "")
              });
            });
            try {
              if (ctx && ctx.layout && typeof ctx.layout.selectPanel === "function") {
                ctx.layout.selectPanel("conversation");
              }
            } catch (e) { /* 忽略布局切换失败 */ }
          },
          exportReport: function () {
            var cards = state.cards && state.cards.data;
            if (!cards) return;
            var md = SYData.exportReportMarkdown(cards);
            if (!md) return;
            var fileName = "shangyi-report-" + (cards.caseId || "case") + ".md";
            try {
              if (typeof document !== "undefined" && typeof Blob !== "undefined" && typeof URL !== "undefined" && typeof URL.createObjectURL === "function") {
                var blob = new Blob([md], { type: "text/markdown;charset=utf-8" });
                var url = URL.createObjectURL(blob);
                var a = document.createElement("a");
                a.href = url;
                a.download = fileName;
                document.body.appendChild(a);
                a.click();
                document.body.removeChild(a);
                setTimeout(function () { URL.revokeObjectURL(url); }, 1000);
              }
              setState(function (prev) {
                return Object.assign({}, prev, { toastMsg: "已导出只读确定性报告：" + fileName });
              });
            } catch (e) {
              setState(function (prev) {
                return Object.assign({}, prev, { toastMsg: "Markdown 下载失败：" + String((e && e.message) || e) });
              });
            }
          },
          printReport: function () {
            try {
              if (typeof window !== "undefined" && typeof window.print === "function") {
                window.print();
              }
            } catch (e) { /* ignore */ }
          }
        };

        var q = state.query.trim().toLowerCase();
        var filtered = q
          ? state.cases.items.filter(function (c) {
            return (c.caseId || "").toLowerCase().indexOf(q) >= 0 ||
              SYData.modeLabel(c.mode).toLowerCase().indexOf(q) >= 0 ||
              (c.phase ? SYData.phaseLabel(c.phase).toLowerCase().indexOf(q) >= 0 : false);
          })
          : state.cases.items;

        var vm = {
          cases: state.cases,
          query: state.query,
          filteredCases: filtered,
          selectedCaseId: state.selectedCaseId,
          cards: state.cards,
          tab: state.tab,
          inspectorOpen: state.inspectorOpen,
          guideOpen: state.guideOpen,
          kb: state.kb,
          kbQuery: state.kbQuery,
          kbKind: state.kbKind,
          health: state.health,
          toastMsg: state.toastMsg,
          cpForm: state.cpForm,
          kbDoc: state.kbDoc,
          actions: actions
        };

        return SYView.renderPanel(vm);
      };
    }
