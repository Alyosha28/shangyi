    // ──────────────────────────────────────────────────────────────
    // 数据层：只做「取 + 收窄形状」，不做任何数值推导。
    // ──────────────────────────────────────────────────────────────
    var SYData = {};

    SYData.CASES_URL = "/api/shangyi/cases";
    SYData.KB_URL = "/api/shangyi/kb";
    SYData.CHECKPOINT_URL = "/api/shangyi/checkpoint";

    SYData.cardsUrl = function (caseId) {
      return "/api/shangyi/cards?case=" + encodeURIComponent(caseId || "latest-cards");
    };

    SYData.kbDocUrl = function (slug) {
      return "/api/shangyi/kb/doc?slug=" + encodeURIComponent(slug || "");
    };

    /** 带 DSH 浏览器鉴权的只读 GET；失败统一抛 Error（组件层转成错误态）。 */
    SYData.getJson = function (url) {
      return fetch(url, { headers: { accept: "application/json" }, cache: "no-store" })
        .then(function (r) {
          if (!r.ok) throw new Error("HTTP " + r.status);
          return r.json();
        })
        .then(function (j) {
          if (!j || j.ok !== true) throw new Error((j && j.error) || "接口返回 ok=false");
          return j;
        });
    };

    /** 受控真人关口确认 POST（Option 1A 通道一：仅限 Web 工作台真人操作触发）。 */
    SYData.postCheckpoint = function (payload) {
      return fetch(SYData.CHECKPOINT_URL, {
        method: "POST",
        headers: { "content-type": "application/json", accept: "application/json" },
        cache: "no-store",
        body: JSON.stringify(payload || {})
      })
        .then(function (r) {
          return r.json().catch(function () { return null; }).then(function (j) {
            if (!r.ok || !j || j.ok !== true) {
              throw new Error((j && j.error) || ("HTTP " + r.status));
            }
            return j;
          });
        });
    };

    /** 阶段 id → 中文。后端 SessionPhase 的 10 个规范 id（phase_graph.rs）。 */
    SYData.PHASE_LABELS = {
      intake: "受理",
      intel: "情报",
      model_build: "建模",
      solve: "求解",
      select: "选择",
      consequence: "后果",
      discover: "发现",
      recommend: "建议",
      report: "报告",
      archived: "归档"
    };

    SYData.MODE_LABELS = {
      scenario: "情景推演",
      whatif: "后果推演",
      discover: "策略发现",
      consult: "咨询",
      research: "研究",
      cost: "费用测算"
    };

    SYData.PLAYBOOK_LABELS = {
      pricing_response: "调价 / 促销响应",
      market_entry: "市场进入",
      negotiation: "谈判对策",
      competitor_intel: "竞对情报"
    };

    SYData.EQ_KIND_LABELS = {
      Pure: "纯策略",
      pure: "纯策略",
      Mixed: "混合策略",
      mixed: "混合策略"
    };

    SYData.SOLVE_ROUTE_LABELS = {
      SupportEnumeration: "支持枚举（精确）",
      LemkeHowson: "Lemke–Howson（交叉检查）",
      Analytic: "解析求根"
    };

    SYData.COMPLETENESS_LABELS = {
      Complete: "完备",
      Partial: "不完备",
      NotGuaranteed: "不保证完备"
    };

    SYData.playbookLabel = function (v) {
      return v ? (SYData.PLAYBOOK_LABELS[v] || v) : v;
    };

    SYData.eqKindLabel = function (v) {
      return v ? (SYData.EQ_KIND_LABELS[v] || v) : v;
    };

    SYData.solveRouteLabel = function (v) {
      return v ? (SYData.SOLVE_ROUTE_LABELS[v] || v) : v;
    };

    SYData.completenessLabel = function (v) {
      if (!v) return v;
      if (SYData.COMPLETENESS_LABELS[v]) return SYData.COMPLETENESS_LABELS[v];
      var s = String(v);
      if (s.indexOf("Partial") === 0) {
        var m = s.match(/unsupported:\s*(\d+),\s*underdetermined:\s*(\d+),\s*ghost:\s*(\d+)/);
        if (m) return "部分完备（超范围 " + m[1] + " · 欠定 " + m[2] + " · 幽灵 " + m[3] + "）";
        return "部分完备";
      }
      if (s.indexOf("NotGuaranteed") === 0) {
        var am = s.match(/algorithm:\s*"([^"]+)"/);
        return am ? "不保证完备（" + am[1] + "）" : "不保证完备";
      }
      return s;
    };

    SYData.SCOPE_LABELS = {
      consultation: "完整博弈咨询",
      research: "调研快答",
      cost: "费用测算",
      "task_scope:full_consult": "完整博弈咨询"
    };

    SYData.phaseLabel = function (id) {
      if (!id) return "—";
      return SYData.PHASE_LABELS[id] || String(id);
    };

    SYData.modeLabel = function (id) {
      if (!id) return "未知模式";
      return SYData.MODE_LABELS[id] || String(id);
    };

    SYData.scopeLabel = function (id) {
      if (!id) return "—";
      return SYData.SCOPE_LABELS[id] || String(id);
    };

    /** /api/shangyi/cases → 前端列表项（字段缺失一律容忍：host-data 还在补 phase/status）。 */
    SYData.normalizeCases = function (json) {
      var raw = (json && Array.isArray(json.cases)) ? json.cases : [];
      var items = [];
      for (var i = 0; i < raw.length; i++) {
        var c = raw[i] || {};
        var caseId = c.caseId || c.case_id || "";
        if (!caseId) continue;
        items.push({
          caseId: String(caseId),
          mtime: c.mtime || null,
          mode: c.mode || null,
          scope: c.scope || null,
          events: typeof c.events === "number" ? c.events : null,
          phase: c.phase || null,
          status: c.status || null,
          pendingCheckpoint: c.pendingCheckpoint || null,
          hasMatrix: c.hasMatrix === true,
          hasSolve: c.hasSolve === true,
          hasSelection: c.hasSelection === true
        });
      }
      return items;
    };

    /** /api/shangyi/cards → 工作台用的收窄视图；matrix/solve/selection 原样保留。 */
    SYData.normalizeCards = function (json) {
      if (!json || typeof json !== "object") throw new Error("案例数据为空");
      var meta = json.meta || {};
      var source = json.source || {};
      var detail = json.pendingCheckpointDetail && typeof json.pendingCheckpointDetail === "object"
        ? {
          id: String(json.pendingCheckpointDetail.id || ""),
          phase: json.pendingCheckpointDetail.phase || null,
          items: Array.isArray(json.pendingCheckpointDetail.items)
            ? json.pendingCheckpointDetail.items.map(function (x) { return String(x); })
            : [],
          at: json.pendingCheckpointDetail.at || null
        }
        : null;
      return {
        requested: json.requested || null,
        caseId: source.caseId || meta.case_id || null,
        source: {
          path: source.path || null,
          caseId: source.caseId || null,
          mtime: source.mtime || null,
          badLines: typeof source.badLines === "number" ? source.badLines : null
        },
        meta: {
          caseId: meta.case_id || source.caseId || null,
          mode: meta.mode || null,
          playbook: meta.playbook || null,
          createdAt: meta.created_at || null,
          events: typeof meta.events === "number" ? meta.events : null,
          turns: typeof meta.turns === "number" ? meta.turns : null,
          tokens: typeof meta.tokens === "number" ? meta.tokens : null,
          tools: typeof meta.tools === "number" ? meta.tools : null,
          scope: meta.scope || null,
          phases: Array.isArray(meta.phases) ? meta.phases.filter(Boolean) : [],
          checkpoints: Array.isArray(meta.checkpoints) ? meta.checkpoints.filter(Boolean) : []
        },
        pendingCheckpoint: json.pendingCheckpoint || (detail ? detail.id : null),
        pendingCheckpointDetail: detail,
        matrix: json.matrix || null,
        solve: json.solve || null,
        selection: json.selection || null,
        failures: Array.isArray(json.failures) ? json.failures : []
      };
    };

    /** 选择分布：equilibrium_id → 概率（0..1）。前端只读，不做归一化。 */
    SYData.selectionIndex = function (selection) {
      var out = {};
      if (!selection || !Array.isArray(selection.entries)) return out;
      for (var i = 0; i < selection.entries.length; i++) {
        var e = selection.entries[i] || {};
        if (e.equilibrium_id && typeof e.prob === "number") out[e.equilibrium_id] = e.prob;
      }
      return out;
    };

    /** 案例阶段链：日志里的 phase_entered 顺序即「已经走过的路」，最后一个就是当前阶段。 */
    SYData.phaseChain = function (cards) {
      var phases = (cards && cards.meta && cards.meta.phases) ? cards.meta.phases : [];
      var out = [];
      for (var i = 0; i < phases.length; i++) {
        out.push({
          id: phases[i],
          label: SYData.phaseLabel(phases[i]),
          current: i === phases.length - 1,
          done: i < phases.length - 1
        });
      }
      return out;
    };

    // ── 格式化（只格式化，不换算法） ──
    SYData.fmtNum = function (n) {
      if (typeof n !== "number" || !isFinite(n)) return "—";
      return n.toLocaleString("zh-CN");
    };

    SYData.fmtTime = function (iso) {
      if (!iso) return "—";
      var d = new Date(iso);
      if (isNaN(d.getTime())) return String(iso);
      function p(x) { return x < 10 ? "0" + x : String(x); }
      return d.getFullYear() + "-" + p(d.getMonth() + 1) + "-" + p(d.getDate()) +
        " " + p(d.getHours()) + ":" + p(d.getMinutes());
    };

    /** 案例 id 太长时缩到中段，保证列表里能看到日期与尾号。 */
    SYData.shortCaseId = function (id) {
      if (!id) return "—";
      var s = String(id);
      if (s.length <= 24) return s;
      return s.slice(0, 12) + "…" + s.slice(-8);
    };
