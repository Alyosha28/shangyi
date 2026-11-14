    // ──────────────────────────────────────────────────────────────
    // 数据层：只做「取 + 收窄形状」，不做任何数值推导。
    // ──────────────────────────────────────────────────────────────
    var SYData = {};

    SYData.CASES_URL = "/api/shangyi/cases";
    SYData.KB_URL = "/api/shangyi/kb";
    SYData.CHECKPOINT_URL = "/api/shangyi/checkpoint";
    SYData.HEALTH_URL = "/api/shangyi/health";

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

    /** 内核健康检查 GET：保留 ok=false 时的 problems 诊断负载以便前端强提示。 */
    SYData.getHealth = function () {
      return fetch(SYData.HEALTH_URL, { headers: { accept: "application/json" }, cache: "no-store" })
        .then(function (r) {
          return r.json().catch(function () { return null; }).then(function (j) {
            if (!r.ok && (!j || !Array.isArray(j.problems))) {
              throw new Error((j && j.error) || ("HTTP " + r.status));
            }
            if (!j || typeof j !== "object") throw new Error("健康检查响应为空");
            return j;
          });
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
      research_and_cost: "调研与费用测算",
      conditional: "条件分析（证据未齐）",
      "task_scope:full_consult": "完整博弈咨询"
    };

    SYData.GAME_FORM_LABELS = {
      normal: "同步博弈（策略式）",
      Normal: "同步博弈（策略式）",
      extensive: "序贯博弈（扩展式）",
      Extensive: "序贯博弈（扩展式）"
    };

    SYData.TOOL_STATUS_LABELS = {
      ok: "正常",
      failed: "失败",
      blocked: "未取得",
      unknown: "结局未知"
    };

    SYData.CHECKPOINT_LABELS = {
      cp1: "关口 1 · 立案边界（cp1）",
      cp2: "关口 2 · 博弈模型与证据（cp2）",
      cp3: "关口 3 · 最终建议核准（cp3）"
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

    SYData.gameFormLabel = function (id) {
      if (!id) return "—";
      return SYData.GAME_FORM_LABELS[id] || String(id);
    };

    SYData.toolStatusLabel = function (id) {
      if (!id) return "—";
      return SYData.TOOL_STATUS_LABELS[id] || String(id);
    };

    SYData.checkpointLabel = function (id) {
      if (!id) return "—";
      return SYData.CHECKPOINT_LABELS[id] || String(id);
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

    SYData.PARAM_SOURCE_LABELS = {
      user_confirmed: "用户当面确认",
      confirmed_assumption: "已确认情景假设",
      public_data: "公开数据",
      industry_prior: "行业先验（非本案实证）",
      assumed: "未确认假设（禁止入解）",
      derived: "引擎派生",
      unknown: "未标注来源"
    };

    SYData.PARAM_SOURCE_TONES = {
      user_confirmed: "ok",
      confirmed_assumption: "ok",
      public_data: "ok",
      industry_prior: "warn",
      assumed: "err",
      derived: "mute",
      unknown: "err"
    };

    SYData.COST_TREATMENT_LABELS = {
      cash_expense: "现金支出",
      refundable_deposit: "可退押金（资金占用）",
      opportunity_cost: "机会成本（非现金）",
      noncash_expense: "非现金账面项",
      unknown: "性质待定（未归类）"
    };

    SYData.PERIOD_LABELS = {
      once: "一次性",
      day: "按日",
      week: "按周",
      month: "按月",
      quarter: "按季",
      year: "按年"
    };

    SYData.KB_KIND_LABELS = {
      theory: "核心理论",
      method: "研究方法",
      concept: "决策概念"
    };

    SYData.ONBOARDING_EXAMPLES = [
      {
        id: "pricing",
        tag: "定价博弈",
        title: "新竞对 9.9 元低价抢客，我跟不跟降？",
        hint: "输出双方支付矩阵、纳什均衡概率与跟降/稳价条件后果",
        prompt: "商圈隔壁新开了一家主打 9.9 元团购的连锁现制茶饮店，我家单杯售价 18 元、毛利 11 元、月固定房租人工 3.8 万，要不要跟进降价还是推会员复购？请帮我做完整博弈推演。"
      },
      {
        id: "location_cost",
        tag: "费用精算",
        title: "双铺位开店选址与硬底线成本核账",
        hint: "严格分清一次性现金、可退押金与缺项（未取得绝不补 0）",
        prompt: "我想在大学城开一家 24 小时自习室，看中两个铺位：A 铺月租 1.4 万、押三付一、转让费 9 万；B 铺月租 9 千、无转让费但消防改造报价待核。手头可用预算 18 万，请先帮我做成本精算与选址补证清单。"
      },
      {
        id: "negotiation",
        tag: "谈判对策",
        title: "大客户年底续约砍价 25%，测算真假虚晃",
        hint: "评估切换成本与承诺可信度，找到我方守住底线的均衡报价带",
        prompt: "一家占我司 30% 营收的 SaaS 老客户年底续约要求降价 25%，否则声称切到竞品；我方与客户切换迁移成本都很高。请分析对方虚张声势的概率与我的让步底线。"
      },
      {
        id: "intel",
        tag: "情报快查",
        title: "竞品突然取消大促、收缩一线城市信号拆解",
        hint: "先区分公开实证信号与未核假设，给出下一步核查清单",
        prompt: "头部竞品近期突然取消大促并关闭两家一线旗舰店，这更可能是现金流承压收缩，还是转向高端差异化提价？请只基于可验证信号给出判断边界与待核清单。"
      }
    ];

    /** /api/shangyi/cards → 工作台用的完整只读视图；不推导、不回填缺失数字。 */
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
      var rawScopeDetail = (json.scopeDetail && typeof json.scopeDetail === "object") ? json.scopeDetail : {};
      var rawCosts = (json.costs && typeof json.costs === "object") ? json.costs : {};
      var rawAssumptions = (json.assumptions && typeof json.assumptions === "object") ? json.assumptions : {};
      var rawEvidence = (json.evidence && typeof json.evidence === "object") ? json.evidence : {};
      var rawConclusion = (json.conclusion && typeof json.conclusion === "object") ? json.conclusion : {};

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
        failures: Array.isArray(json.failures) ? json.failures : [],
        scopeDetail: {
          scope: rawScopeDetail.scope || meta.scope || null,
          reason: rawScopeDetail.reason || null,
          scopedResult: rawScopeDetail.scopedResult || null,
          conditional: rawScopeDetail.conditional || null
        },
        costs: {
          scenarios: Array.isArray(rawCosts.scenarios) ? rawCosts.scenarios : [],
          constraints: Array.isArray(rawCosts.constraints) ? rawCosts.constraints : [],
          constraintCheck: rawCosts.constraintCheck || null,
          missingItems: Array.isArray(rawCosts.missingItems) ? rawCosts.missingItems : []
        },
        assumptions: {
          players: Array.isArray(rawAssumptions.players) ? rawAssumptions.players : [],
          params: Array.isArray(rawAssumptions.params) ? rawAssumptions.params : [],
          counts: rawAssumptions.counts || {},
          gaps: Array.isArray(rawAssumptions.gaps) ? rawAssumptions.gaps : []
        },
        evidence: {
          checkpoints: Array.isArray(rawEvidence.checkpoints) ? rawEvidence.checkpoints : [],
          sources: Array.isArray(rawEvidence.sources) ? rawEvidence.sources : [],
          poiSurveys: Array.isArray(rawEvidence.poiSurveys) ? rawEvidence.poiSurveys : [],
          researchRecords: Array.isArray(rawEvidence.researchRecords) ? rawEvidence.researchRecords : [],
          kbQueries: Array.isArray(rawEvidence.kbQueries) ? rawEvidence.kbQueries : [],
          tools: Array.isArray(rawEvidence.tools) ? rawEvidence.tools : [],
          exploratoryCalls: typeof rawEvidence.exploratoryCalls === "number" ? rawEvidence.exploratoryCalls : 0
        },
        conclusion: {
          headline: rawConclusion.headline || null,
          recommend: rawConclusion.recommend || null,
          consequence: rawConclusion.consequence || null,
          discovery: rawConclusion.discovery || null,
          reportModel: rawConclusion.reportModel || null,
          hasEngineResult: Boolean(rawConclusion.hasEngineResult),
          hasExploratoryOnly: Boolean(rawConclusion.hasExploratoryOnly)
        }
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

    // ── 格式化（只格式化，不换算法；null 永不写成 0） ──
    SYData.fmtNum = function (n) {
      if (typeof n !== "number" || !isFinite(n)) return "—";
      return n.toLocaleString("zh-CN");
    };

    SYData.fmtCostValue = function (v, currency) {
      if (v === null || v === undefined || v === "") return "未取得";
      if (typeof v === "number" && isFinite(v)) {
        return v.toLocaleString("zh-CN") + (currency ? (" " + currency) : "");
      }
      return String(v);
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

    SYData.paramSourceLabel = function (kind) {
      return SYData.PARAM_SOURCE_LABELS[kind] || String(kind || "未标注");
    };

    SYData.paramSourceTone = function (kind) {
      return SYData.PARAM_SOURCE_TONES[kind] || "mute";
    };

    SYData.costTreatmentLabel = function (t) {
      return SYData.COST_TREATMENT_LABELS[t] || String(t || "—");
    };

    SYData.periodLabel = function (p) {
      return SYData.PERIOD_LABELS[p] || String(p || "—");
    };

    SYData.safeHref = function (raw) {
      if (!raw || typeof raw !== "string") return null;
      var s = raw.trim();
      if (/^https?:\/\//i.test(s)) return s;
      return null;
    };

    /** 把 Markdown 里的 [[wiki-link]] 解析为知识库 slug（匹配 slug / id / title / 横线下划线等价）。 */
    SYData.resolveKbSlug = function (target, items) {
      var clean = String(target || "").trim().split("#")[0].replace(/^\.\//, "").replace(/\.md$/i, "");
      if (!clean) return "";
      var list = Array.isArray(items) ? items : [];
      var i;
      for (i = 0; i < list.length; i++) {
        if (list[i] && list[i].slug === clean) return list[i].slug;
      }
      for (i = 0; i < list.length; i++) {
        if (list[i] && (list[i].id === clean || list[i].title === clean)) return list[i].slug || clean;
      }
      var norm = clean.toLowerCase().replace(/[\s_]+/g, "-");
      for (i = 0; i < list.length; i++) {
        var d = list[i] || {};
        var ds = String(d.slug || "").toLowerCase().replace(/[\s_]+/g, "-");
        var di = String(d.id || "").toLowerCase().replace(/[\s_]+/g, "-");
        var dt = String(d.title || "").toLowerCase().replace(/[\s_]+/g, "-");
        if (ds === norm || di === norm || dt === norm) return d.slug || clean;
      }
      return clean;
    };

    /** 解析知识库 front-matter 与正文，避免把裸 YAML 当普通文本显示。 */
    SYData.parseFrontMatterAndBody = function (markdown) {
      var text = String(markdown || "").replace(/\r\n/g, "\n");
      var fm = {};
      var body = text;
      if (text.indexOf("---\n") === 0) {
        var end = text.indexOf("\n---", 3);
        if (end >= 0) {
          var lines = text.slice(4, end).split("\n");
          for (var i = 0; i < lines.length; i++) {
            var m = /^([A-Za-z_][\w-]*):[ \t]*(.*)$/.exec(lines[i].trim());
            if (m) {
              var v = m[2].trim();
              if ((v.charAt(0) === '"' && v.charAt(v.length - 1) === '"') ||
                  (v.charAt(0) === "'" && v.charAt(v.length - 1) === "'")) {
                v = v.slice(1, -1);
              }
              fm[m[1]] = v;
            }
          }
          body = text.slice(end + 4).replace(/^\n+/, "");
        }
      }
      return { meta: fm, body: body };
    };

    /** 行内 Markdown AST（纯文本节点 / strong / code / safe link / wiki-link），绝不用 dangerouslySetInnerHTML。 */
    SYData.parseInlineTokens = function (line) {
      var s = String(line || "");
      var out = [];
      var pos = 0;
      var re = /(\[\[([^\]|]+?)(?:\|([^\]]+?))?\]\]|`([^`]+)`|\*\*([^*]+)\*\*|\[([^\]]+)\]\(([^)]+)\))/g;
      var m;
      while ((m = re.exec(s)) !== null) {
        if (m.index > pos) out.push({ type: "text", text: s.slice(pos, m.index) });
        if (m[2] !== undefined) {
          out.push({ type: "wiki", target: m[2].trim(), label: (m[3] || m[2]).trim() });
        } else if (m[4] !== undefined) {
          out.push({ type: "code", text: m[4] });
        } else if (m[5] !== undefined) {
          out.push({ type: "bold", text: m[5] });
        } else if (m[6] !== undefined && m[7] !== undefined) {
          var safe = SYData.safeHref(m[7]);
          if (safe) out.push({ type: "link", text: m[6], href: safe });
          else out.push({ type: "text", text: m[6] + " (" + m[7] + ")" });
        }
        pos = re.lastIndex;
      }
      if (pos < s.length) out.push({ type: "text", text: s.slice(pos) });
      return out;
    };

    /** 块级 Markdown AST：支持标题、无序/有序列表、引用、代码块、表格、段落。 */
    SYData.parseMarkdownBlocks = function (markdown) {
      var lines = String(markdown || "").replace(/\r\n/g, "\n").split("\n");
      var blocks = [];
      var i = 0;
      while (i < lines.length) {
        var line = lines[i];
        var trimmed = line.trim();
        if (!trimmed) { i++; continue; }

        if (/^```/.test(trimmed)) {
          var lang = trimmed.slice(3).trim();
          var codeLines = [];
          i++;
          while (i < lines.length && !/^```/.test(lines[i].trim())) {
            codeLines.push(lines[i]);
            i++;
          }
          if (i < lines.length) i++;
          blocks.push({ type: "code", lang: lang, text: codeLines.join("\n") });
          continue;
        }

        var hm = /^(#{1,4})[ \t]+(.+)$/.exec(trimmed);
        if (hm) {
          blocks.push({ type: "heading", level: hm[1].length, text: hm[2].trim() });
          i++;
          continue;
        }

        if (/^---+$/.test(trimmed) || /^\*\*\*+$/.test(trimmed)) {
          blocks.push({ type: "hr" });
          i++;
          continue;
        }

        if (/^>\s?/.test(trimmed)) {
          var ql = [];
          while (i < lines.length && /^>\s?/.test(lines[i].trim())) {
            ql.push(lines[i].trim().replace(/^>\s?/, ""));
            i++;
          }
          blocks.push({ type: "quote", text: ql.join("\n") });
          continue;
        }

        if (trimmed.indexOf("|") === 0 && i + 1 < lines.length && /^\|[\s:|-]+\|?$/.test(lines[i + 1].trim())) {
          function splitRow(r) {
            return r.trim().replace(/^\|/, "").replace(/\|$/, "").split("|").map(function (c) { return c.trim(); });
          }
          var headers = splitRow(trimmed);
          i += 2;
          var rows = [];
          while (i < lines.length && lines[i].trim().indexOf("|") >= 0 && lines[i].trim() !== "") {
            rows.push(splitRow(lines[i]));
            i++;
          }
          blocks.push({ type: "table", headers: headers, rows: rows });
          continue;
        }

        if (/^[-*+]\s+/.test(trimmed)) {
          var ulItems = [];
          while (i < lines.length && /^[-*+]\s+/.test(lines[i].trim())) {
            ulItems.push(lines[i].trim().replace(/^[-*+]\s+/, ""));
            i++;
          }
          blocks.push({ type: "ul", items: ulItems });
          continue;
        }

        if (/^\d+\.\s+/.test(trimmed)) {
          var olItems = [];
          while (i < lines.length && /^\d+\.\s+/.test(lines[i].trim())) {
            olItems.push(lines[i].trim().replace(/^\d+\.\s+/, ""));
            i++;
          }
          blocks.push({ type: "ol", items: olItems });
          continue;
        }

        var para = [];
        while (i < lines.length) {
          var cur = lines[i].trim();
          if (!cur || /^```/.test(cur) || /^#{1,4}[ \t]+/.test(cur) || /^>\s?/.test(cur) ||
              /^[-*+]\s+/.test(cur) || /^\d+\.\s+/.test(cur) ||
              (cur.indexOf("|") === 0 && i + 1 < lines.length && /^\|[\s:|-]+\|?$/.test(lines[i + 1].trim()))) {
            break;
          }
          para.push(cur);
          i++;
        }
        if (para.length) blocks.push({ type: "p", text: para.join(" ") });
      }
      return blocks;
    };

    /** 按严格铁律 1 导出当前案例只读 Markdown 报告：绝不重算、绝不把 null 成本填 0、绝不输出 sandbox_exec 数字。 */
    SYData.exportReportMarkdown = function (cards) {
      if (!cards) return "";
      var lines = [];
      var m = cards.meta || {};
      var conc = cards.conclusion || {};
      lines.push("# 商奕决策报告：" + (cards.caseId || m.caseId || "未命名案例"));
      lines.push("");
      lines.push("- **案例 ID**：`" + (cards.caseId || m.caseId || "—") + "`");
      lines.push("- **模式 / 剧本 / 范围**：`" + SYData.modeLabel(m.mode) + "` / `" + (SYData.playbookLabel(m.playbook) || "未绑定") + "` / `" + SYData.scopeLabel(m.scope) + "`");
      lines.push("- **证据日志**：`" + ((cards.source && cards.source.path) || "—") + "`");
      if (cards.evidence && cards.evidence.exploratoryCalls > 0) {
        lines.push("");
        lines.push("> **⚠ 探索性工具隔离声明**：本案例曾有 " + cards.evidence.exploratoryCalls + " 次 `sandbox_exec`（`TrustLevel::Exploratory`）调用，已全部隔离排除出本报告数字。");
      }
      lines.push("");
      lines.push("## 一、白话结论与下一步行动");
      lines.push("");
      if (conc.headline) {
        lines.push("**核心结论**：" + conc.headline);
        lines.push("");
      }
      if (conc.recommend && Array.isArray(conc.recommend.actions) && conc.recommend.actions.length) {
        lines.push("### 建议推进动作");
        lines.push("");
        for (var ai = 0; ai < conc.recommend.actions.length; ai++) {
          var act = conc.recommend.actions[ai] || {};
          lines.push((ai + 1) + ". **" + (act.action || "") + "**" + (act.rationale ? (" — 依据：" + act.rationale) : ""));
        }
        lines.push("");
      }
      if (conc.recommend && Array.isArray(conc.recommend.do_not) && conc.recommend.do_not.length) {
        lines.push("### 明确不建议（底线避坑）");
        lines.push("");
        for (var di = 0; di < conc.recommend.do_not.length; di++) lines.push("- " + conc.recommend.do_not[di]);
        lines.push("");
      }
      if (cards.costs && Array.isArray(cards.costs.scenarios) && cards.costs.scenarios.length) {
        lines.push("## 二、成本精算与硬约束");
        lines.push("");
        for (var ci = 0; ci < cards.costs.scenarios.length; ci++) {
          var sc = cards.costs.scenarios[ci] || {};
          lines.push("### 方案：" + (sc.scenario || sc.scenario_id || ("方案 " + (ci + 1))));
          lines.push("- 期初现金总需求（含押金）：`" + SYData.fmtCostValue(sc.listed_upfront_cash, sc.currency) + "`");
          lines.push("- 一次性现金支出：`" + SYData.fmtCostValue(sc.listed_upfront_cash_expense, sc.currency) + "`");
          lines.push("- 可退押金：`" + SYData.fmtCostValue(sc.listed_refundable_deposits, sc.currency) + "`");
          lines.push("- 月固定现金支出：`" + SYData.fmtCostValue(sc.listed_monthly_fixed_cost, sc.currency) + "`");
          if (Array.isArray(sc.items) && sc.items.length) {
            lines.push("");
            lines.push("| 费用项目 | 性质分类 | 周期 | 标准化金额 | 来源 | 缺项说明 |");
            lines.push("|---|---|---|---|---|---|");
            for (var ii = 0; ii < sc.items.length; ii++) {
              var it = sc.items[ii] || {};
              lines.push("| " + (it.name || "—") + " | " + SYData.costTreatmentLabel(it.treatment) + " | " + SYData.periodLabel(it.normalized_period) + " | `" + SYData.fmtCostValue(it.normalized_amount, sc.currency) + "` | " + (it.source || "—") + " | " + (it.missing_amount_reason || "") + " |");
            }
          }
          lines.push("");
        }
      }
      if (cards.assumptions && Array.isArray(cards.assumptions.params) && cards.assumptions.params.length) {
        lines.push("## 三、关键假设与四类证据来源");
        lines.push("");
        lines.push("| 参数 | 值/引用 | 单位 | 来源分类 | 来源明细 |");
        lines.push("|---|---|---|---|---|");
        for (var pi = 0; pi < cards.assumptions.params.length; pi++) {
          var p = cards.assumptions.params[pi] || {};
          lines.push("| " + (p.name || p.key) + " | `" + (p.valueText || "—") + "` | " + (p.unit || "—") + " | " + SYData.paramSourceLabel(p.sourceKind) + " | " + (p.sourceDetail || "") + " |");
        }
        lines.push("");
      }
      if (cards.assumptions && Array.isArray(cards.assumptions.gaps) && cards.assumptions.gaps.length) {
        lines.push("## 四、尚未核清与证据缺口");
        lines.push("");
        for (var gi = 0; gi < cards.assumptions.gaps.length; gi++) {
          lines.push("- " + cards.assumptions.gaps[gi]);
        }
        lines.push("");
      }
      if (cards.evidence && Array.isArray(cards.evidence.sources) && cards.evidence.sources.length) {
        lines.push("## 五、公开外部来源");
        lines.push("");
        for (var si = 0; si < cards.evidence.sources.length; si++) {
          var src = cards.evidence.sources[si] || {};
          lines.push("- " + (src.title ? (src.title + "：") : "") + (src.url || ""));
        }
        lines.push("");
      }
      return lines.join("\n");
    };
