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
          kb: { phase: "loading", count: 0, items: [] },
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

        // 首次进入：抓案例列表 + 知识库目录（一次）。
        React.useEffect(function () {
          loadCases();
          loadKb();
        }, [loadCases, loadKb]);

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
              if (prev.selectedCaseId === caseId) return prev;
              return Object.assign({}, prev, {
                selectedCaseId: caseId,
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
          openKbDoc: function (slug) {
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
            setState(function (prev) { return Object.assign({}, prev, { tab: tab }); });
          },
          toggleInspector: function () {
            setState(function (prev) { return Object.assign({}, prev, { inspectorOpen: !prev.inspectorOpen }); });
          },
          retryCards: function () {
            loadCards(state.selectedCaseId);
          },
          refresh: function () {
            loadCases({ keepSelection: true });
            loadKb();
            if (state.selectedCaseId) loadCards(state.selectedCaseId);
          },
          openConversation: function () {
            try {
              if (ctx && ctx.layout && typeof ctx.layout.selectPanel === "function") {
                ctx.layout.selectPanel(null); // null = 回到对话面板
              }
            } catch (e) {
              // 布局服务不可用时不阻断：用户仍可用侧栏「新会话」切回。
            }
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
          kb: state.kb,
          cpForm: state.cpForm,
          kbDoc: state.kbDoc,
          actions: actions
        };

        return SYView.renderPanel(vm);
      };
    }
