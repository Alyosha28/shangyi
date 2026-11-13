// @ts-check
// 共享渲染组件：支付矩阵、均衡卡片、概率分布、阶段步进、事件时间线、报告。
// 标记层照抄 Tabler 组件（card / badge / timeline / progress / empty），
// 领域可视化（矩阵热力、混合条）是仅有的自定义块。全部返回 HTML 字符串，
// 动态输入一律经 esc()；案例详情与推演台共用同一套渲染器。

import { esc, fmtNum, fmtTime, parseRational, exprValue, phaseLabel, MODE_PHASES, CONF_LABEL } from "./util.js";

// 时间线事件图标（Tabler icons 风格的手写行内 SVG，24×24 / stroke 2）
const I = {
  check: '<svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12l5 5L20 7"/></svg>',
  x: '<svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M6 6l12 12M18 6L6 18"/></svg>',
  warn: '<svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3l10 18H2z"/><path d="M12 10v4"/><path d="M12 17.5h.01"/></svg>',
  activity: '<svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12h4l3-8 4 16 3-8h4"/></svg>',
  diamond: '<svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3l9 9-9 9-9-9z"/></svg>',
  tool: '<svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 6l4 4-9 9H5v-4z"/><path d="M13 5l2-2 4 4-2 2"/></svg>',
};

// ── 阶段步进 ────────────────────────────────────────────────────────────────

export function phaseStepper(mode, phasesReached) {
  const path = MODE_PHASES[mode] || MODE_PHASES.scenario;
  const reached = new Set(phasesReached || []);
  const lastIdx = Math.max(-1, ...path.map((p, i) => (reached.has(p) ? i : -1)));
  const modeCp3 = { scenario: "select", whatif: "consequence", discover: "discover" }[mode];
  const cps = { intel: "cp1", model_build: "cp2", [modeCp3]: "cp3" };
  return `<div class="steps-diy">${path
    .map((p, i) => {
      const cls = i < lastIdx ? "done" : i === lastIdx ? "current" : "";
      const cp = cps[p] ? `<span class="cp">◆${cps[p]}</span>` : "";
      return `<div class="step-diy ${cls}">${cp}<div class="dot"></div><div class="plabel">${phaseLabel(p)}</div></div>`;
    })
    .join("")}</div>`;
}

// ── 支付矩阵（只读展示） ────────────────────────────────────────────────────

export function payoffMatrix(spec, opts = {}) {
  // 行列玩家顺序 = spec.players **数组**顺序（relation_to_game=player 的前两个）。
  // ⚠️ 引擎语义（gamespec validate 与 solver from_gamespec 同款）：cells 键 =
  // "策略按 players 数组顺序以 , 连接"。strategies 是 JSON map（BTree 序），
  // 与 players 数组序在真实案例里可以不同（实测 discover-072136：players=[focal,chain]
  // 而 strategies 键序 chain<focal）——按 strategies 键序读会把整个矩阵读反/读空。
  const playerIds = (spec.players || [])
    .filter((p) => (p.relation_to_game || "player") === "player")
    .map((p) => p.id);
  if (playerIds.length < 2) {
    // 兜底：players 缺失时退回 strategies 键序（推演台自构 spec 两者一致）
    playerIds.length = 0;
    playerIds.push(...Object.keys(spec.strategies || {}));
  }
  if (playerIds.length !== 2) {
    return `<div class="empty"><p class="empty-title">不是两方（${playerIds.length} 方）</p><p class="empty-subtitle text-secondary">这张表只看两方</p></div>
      <details class="raw"><summary>原始数据</summary><pre>${esc(JSON.stringify(spec, null, 2))}</pre></details>`;
  }
  const [a, b] = playerIds;
  const nameOf = (id) => spec.players?.find((p) => p.id === id)?.name || id;
  const stratsA = spec.strategies[a];
  const stratsB = spec.strategies[b];
  const cells = spec.payoffs?.values?.cells || {};
  const params = spec.params || {};

  // focal（行玩家）数值归一化热力：绿正红负
  const vals = [];
  for (const sa of stratsA)
    for (const sb of stratsB) {
      const c = cells[`${sa},${sb}`] || cells[`${sa}|${sb}`];
      if (c) vals.push(exprValue(c[a], params));
    }
  const finite = vals.filter((v) => v != null && Number.isFinite(v));
  const min = finite.length ? Math.min(...finite) : 0;
  const max = finite.length ? Math.max(...finite) : 0;
  const heat = (v) => {
    if (v == null || !Number.isFinite(v) || max === min) return "";
    const t = (v - min) / (max - min);
    const alpha = 0.08 + Math.abs(t - 0.5) * 2 * 0.22;
    return t >= 0.5
      ? `background:rgba(var(--tblr-success-rgb),${alpha.toFixed(3)})`
      : `background:rgba(var(--tblr-danger-rgb),${alpha.toFixed(3)})`;
  };

  let html = `<div class="matrix-wrap"><table class="matrix">
    <tr><th class="corner"></th><th class="corner" colspan="${stratsB.length}">${esc(nameOf(b))} ↓</th></tr>
    <tr><th class="corner">${esc(nameOf(a))} →</th>${stratsB
      .map((sb) => `<th>${esc(sb)}</th>`)
      .join("")}</tr>`;
  for (const sa of stratsA) {
    html += `<tr><th>${esc(sa)}</th>`;
    for (const sb of stratsB) {
      const c = cells[`${sa},${sb}`] || cells[`${sa}|${sb}`];
      const va = c ? exprValue(c[a], params) : null;
      const vb = c ? exprValue(c[b], params) : null;
      const hit = opts.equilibriumCells?.has(`${sa},${sb}`) ? " eq-hit" : "";
      const cellHtml = c
        ? `<span class="pv"><span class="who">${esc(shortName(nameOf(a)))}</span>${fmtNum(va)}</span>
           <span class="pv"><span class="who">${esc(shortName(nameOf(b)))}</span>${fmtNum(vb)}</span>`
        : `<span class="pv text-secondary">缺</span>`;
      html += `<td class="cell${hit}" style="${heat(va)}">${cellHtml}</td>`;
    }
    html += `</tr>`;
  }
  html += `</table></div>`;
  return html;
}

function shortName(n) {
  return String(n).length > 6 ? String(n).slice(0, 5) + "…" : n;
}

/** 参数表 chips。 */
export function paramChips(spec) {
  const params = Object.values(spec?.params || {});
  if (!params.length) return `<span class="text-secondary">没有参数，全是填好的数</span>`;
  return `<div class="chip-row">${params
    .map((p) => {
      const v =
        typeof p.value === "number" ? p.value : p.value?.kind === "literal" ? p.value.value : "?";
      return `<span class="param-chip"><span class="pn">${esc(p.name)}</span><span class="pv">${esc(
        fmtNum(v)
      )}</span>${p.unit ? `<span class="pu">${esc(p.unit)}</span>` : ""}</span>`;
    })
    .join("")}</div>`;
}

// ── 均衡卡片 ────────────────────────────────────────────────────────────────

export function equilibriaCards(sol, strategiesByPlayer, names, opts = {}) {
  if (!sol || !sol.equilibria?.length) {
    return `<div class="empty"><p class="empty-title">没算出能留的做法</p></div>`;
  }
  const colors = ["var(--tblr-success)", "var(--tblr-primary)", "var(--tblr-warning)", "var(--tblr-info)"];
  const pidList = Object.keys(strategiesByPlayer);
  const parts = [];
  for (const eq of sol.equilibria) {
    const supportNames = eq.support.map((sup, pi) => {
      const arr = strategiesByPlayer[pidList[pi]] || [];
      return sup.map((i) => arr[i]).filter(Boolean);
    });
    const mixRows = eq.strategy
      .map((probs, pi) => {
        const pid = pidList[pi];
        const strats = strategiesByPlayer[pid] || [];
        const segs = probs
          .map((p, si) => {
            const { frac, float } = parseRational(p);
            const w = Number.isFinite(float) ? Math.max(float, 0) : 0;
            const zero = float <= 0.001;
            return `<div class="mix-seg" style="flex:0 0 ${(w * 100).toFixed(2) || 0}%; background:${
              colors[si % colors.length]
            }; color:${zero ? "var(--tblr-secondary)" : "#040a11"}" title="${esc(
              strats[si] || si
            )} = ${esc(frac)}">${!zero && w > 0.13 ? esc(frac) : ""}</div>`;
          })
          .join("");
        return `<div class="mix-row"><div class="who" title="${esc(names[pid] || pid)}">${esc(
          names[pid] || pid
        )}</div><div class="mix-bar">${segs}</div></div>`;
      })
      .join("");
    const payoffs = eq.payoffs
      .map((p, i) => {
        const pid = pidList[i];
        return `<span class="num-badge" title="${esc(names[pid] || pid)} 这一情况的好坏">${esc(
          shortName(names[pid] || pid)
        )} <span class="nv">${esc(parseRational(p).frac)}</span></span>`;
      })
      .join(" ");
    parts.push(`
    <div class="card card-sm">
      <div class="card-body">
        <div class="d-flex align-items-center gap-2 mb-2">
          <span class="mono fw-bold" style="color:var(--tblr-primary); font-size:14px">${esc(eq.id)}</span>
          <span class="badge ${eq.kind === "Pure" ? "bg-success-lt" : "bg-purple-lt"}">${
            eq.kind === "Pure" ? "就选一个" : "混着来"
          }</span>
          <span class="ms-auto d-flex gap-1 flex-wrap">${payoffs}</span>
        </div>
        ${mixRows}
        <div class="text-secondary small mt-1">就这几种：${supportNames
          .map((s, i) => `${esc(names[pidList[i]] || pidList[i])} ∈ {${s.map(esc).join(", ")}}`)
          .join("；")}</div>
      </div>
    </div>`);
  }
  const refine = sol.refinement
    ? `<div class="text-secondary small mt-2">再筛（${esc(sol.refinement.method)}）：留下 ${esc(
        (sol.refinement.survivors || []).join("、")
      )}${
        sol.refinement.excluded?.length
          ? `；去掉 ${sol.refinement.excluded.map((e) => `${esc(e.equilibrium)}（${esc(e.reason)}）`).join("、")}`
          : ""
      }</div>`
    : "";
  const head = opts.compact
    ? ""
    : `<div class="chip-row mb-2">
        <span class="badge bg-info-lt">路子 ${esc(sol.route || "?")}</span>
        <span class="badge ${sol.completeness === "Complete" ? "bg-success-lt" : "bg-warning-lt"}">${
          sol.completeness === "Complete" ? "找全了" : esc(sol.completeness)
        }</span>
        <span class="badge bg-secondary-lt">${sol.equilibria.length} 种稳的做法</span>
      </div>`;
  return head + parts.join("") + refine;
}

/** 均衡支撑集 → 矩阵高亮键 "sa,sb"。 */
export function eqCellKeys(sol, strategiesByPlayer, which = "all") {
  const pidList = Object.keys(strategiesByPlayer);
  const keys = new Set();
  const list = which === "first" ? sol?.equilibria?.slice(0, 1) : sol?.equilibria;
  for (const eq of list || []) {
    if (pidList.length !== 2) continue;
    const sa = eq.support[0] || [];
    const sb = eq.support[1] || [];
    const A = strategiesByPlayer[pidList[0]] || [];
    const B = strategiesByPlayer[pidList[1]] || [];
    for (const i of sa) for (const j of sb) keys.add(`${A[i]},${B[j]}`);
  }
  return keys;
}

// ── 均衡分布 ────────────────────────────────────────────────────────────────

export function distributionCard(dist) {
  if (!dist || !dist.entries?.length) return "";
  const rows = dist.entries
    .map(
      (e) => `
    <div class="dist-row">
      <span class="eid">${esc(e.equilibrium_id)}</span>
      <div class="progress progress-sm" style="height:10px">
        <div class="progress-bar" style="width:${(e.prob * 100).toFixed(1)}%; background:var(--tblr-primary)"></div>
      </div>
      <span class="pct">${(e.prob * 100).toFixed(1)}%</span>
    </div>
    <div class="chip-row">${(e.drivers || [])
      .map((d) => `<span class="driver-chip">${esc(d)}</span>`)
      .join("")}</div>`
    )
    .join("");
  const confKey = dist.confidence ? Object.keys(dist.confidence)[0] : null;
  const conf = confKey
    ? `<span class="badge bg-warning-lt" title="${esc(JSON.stringify(dist.confidence[confKey]))}">把握：${esc(
        confKey
      )}（试了 ${dist.confidence[confKey]?.samples ?? "?"}/${dist.confidence[confKey]?.needed ?? "?"} 次）</span>`
    : "";
  return `
  <div class="chip-row mb-2">${conf}${
    dist.divergences?.length
      ? `<span class="badge bg-danger-lt">几种说法不一致 ${dist.divergences.length}</span>`
      : `<span class="badge bg-success-lt">说法一致</span>`
  }</div>
  ${rows}`;
}

// ── 事件时间线（Tabler .timeline 组件） ────────────────────────────────────

const TOOL_CN = {
  kb_retrieve: "查知识库", web_search: "查公开资料", web_fetch: "打开网页",
  case_store_read: "看旧案例", case_store_write: "存案例",
  gamespec_build: "整理成局", gamespec_validate: "检查摆的局",
  solve_equilibria: "算哪些做法稳", select_equilibria: "掂量取舍", select_fuse: "合起来看",
  sensitivity_scan: "动动数字试试", monte_carlo: "随机试很多遍",
  consequence_predict: "做了会怎样", novel_move_filter: "挑新招",
  discover_satisfice: "找够好的做法", participant_profile_update: "更新对对方的印象",
  vn_map: "看还牵连谁", preference_inject: "放进你的偏好", checkpoint: "跟你核对",
  ask_user: "问你问题", request_rewind: "想退回重做", report_render: "生成报告",
  recommend_build: "整理建议", swarm_run: "演一遍很多人", sandbox_exec: "试跑代码",
  pars_check: "PARTS 检查", data_ingest: "数据导入",
};

export function timeline(events, opts = {}) {
  if (!events?.length) return `<div class="empty"><p class="empty-title">还没记录</p></div>`;
  const showToken = opts.showToken !== false;
  const items = [];
  for (const ev of events) {
    const t = ev.type;
    if (t === "token_usage" && !showToken) continue;
    if (t === "step_started" || t === "step_ended" || t === "turn_ended") continue;
    items.push(ev);
  }
  let html = `<div class="timeline">`;
  for (const ev of items) {
    const at = fmtTime(ev.at).slice(11);
    let icon = I.activity, tone = "";
    let main = "";
    let detail = "";
    switch (ev.type) {
      case "turn_started":
        tone = "";
        main = `回合 <b>#${ev.turn}</b> 开始`;
        break;
      case "phase_entered":
        tone = "primary";
        main = `进入阶段 <b>${phaseLabel(ev.to)}</b>${ev.reason ? ` · ${esc(ev.reason)}` : ""}`;
        break;
      case "phase_approved":
        tone = "primary";
        main = `阶段 <b>${phaseLabel(ev.phase)}</b> 核对通过（校验码 <span class="mono">${esc(ev.fingerprint)}</span>）`;
        break;
      case "phase_rewound":
        tone = "danger"; icon = I.warn;
        main = `退回去：<b>${phaseLabel(ev.to ?? ev.phase)}</b>`;
        break;
      case "checkpoint_opened":
        tone = "warning"; icon = I.diamond;
        main = `核对点 <b>${esc(ev.id)}</b> 开始（${(ev.items || []).length} 项）`;
        detail = (ev.items || []).map((i) => `· ${i}`).join("\n");
        break;
      case "checkpoint_answered":
        tone = "warning"; icon = I.diamond;
        {
          const actor = ev.user_actor === "auto" ? "自动" : "用户";
          main = `核对点 <b>${esc(ev.id)}</b> 答了（${actor}）`;
          detail = (ev.answers || []).map((a) => `· ${a}`).join("\n");
        }
        break;
      case "tool_call_started":
        tone = "secondary"; icon = I.tool;
        main = `用了 <b>${esc(TOOL_CN[ev.name] || ev.name)}</b>`;
        if (ev.name === "gamespec_build" || ev.name === "solve_equilibria") {
          try {
            detail = JSON.stringify(JSON.parse(ev.arguments || "{}"), null, 1);
          } catch {
            detail = ev.arguments || "";
          }
        }
        break;
      case "tool_call_finished":
        {
          const st = ev.outcome?.status || "unknown";
          icon = st === "ok" ? I.check : st === "blocked" ? I.warn : I.x;
          tone = st === "ok" ? "success" : st === "blocked" ? "warning" : "danger";
          const nm = TOOL_CN[ev.name] || ev.name;
          main = `<b>${esc(nm)}</b> → ${st === "ok" ? "完成" : st === "blocked" ? "被拦下" : st === "error" ? "出错" : esc(st)}`;
          const out = ev.outcome?.output || "";
          if (out.length < 4000) {
            let pretty = out;
            try {
              pretty = JSON.stringify(JSON.parse(out), null, 1);
            } catch {}
            detail = pretty;
          } else {
            detail = out.slice(0, 4000) + `\n…（共 ${out.length} 字符，已截断）`;
          }
        }
        break;
      case "token_usage":
        tone = "secondary"; icon = I.activity;
        main = `token：<span class="mono">${esc(ev.model || "")}</span> in ${esc(ev.input_tokens)} / out ${esc(
          ev.output_tokens
        )}${ev.reasoning_tokens ? ` / 推理 ${ev.reasoning_tokens}` : ""}${
          ev.cached_input_tokens ? `（缓存 ${ev.cached_input_tokens}）` : ""
        }`;
        break;
      case "warning":
        tone = "danger"; icon = I.warn;
        main = `${esc(ev.message || ev.reason || "警告")}`;
        break;
      default:
        main = esc(ev.type);
    }
    html += `
    <div class="timeline-event">
      <div class="timeline-event-icon${tone ? ` timeline-event-icon-${tone}` : ""}">${icon}</div>
      <div class="card card-sm"><div class="card-body py-2">
        <div class="d-flex align-items-baseline gap-2">
          <span class="mono text-secondary" style="font-size:11px">${at}</span>
          <span class="small">${main}</span>
        </div>
        ${detail ? `<div class="tl-detail">${esc(detail)}</div>` : ""}
      </div></div>
    </div>`;
  }
  html += `</div>`;
  return html;
}

// ── 报告渲染 ────────────────────────────────────────────────────────────────

const LAYER_CN = { boss: "给你的摘要", scenario: "情况和稳的做法", profile: "对对方的印象", appendix: "附录" };

export function reportCard(reportModel) {
  const rep = reportModel?.report || reportModel;
  if (!rep?.sections) return `<div class="empty"><p class="empty-title">还没报告</p></div>`;
  const numbers = rep.numbers || {};
  const ledger = rep.ledger?.records || {};
  const provKind = {
    "code-computed": "引擎计算",
    "user-confirmed": "用户确认",
    exploratory: "试算的，不进报告",
  };
  const sectionsHtml = rep.sections
    .map((s) => {
      const nums = (s.number_keys || [])
        .map((k) => numbers[k])
        .filter(Boolean)
        .map(
          (n) =>
            `<span class="num-badge" title="来源：${esc(
              provKind[ledger[n.provenance]?.kind] || n.provenance
            )}">${esc(n.label)} <span class="nv">${esc(fmtNum(n.value))}</span>${
              n.unit ? ` ${esc(n.unit)}` : ""
            }</span>`
        )
        .join(" ");
      return `
      <div class="report-section l-${esc(s.layer)}">
        <span class="layer-tag">◇ ${esc(LAYER_CN[s.layer] || s.layer)}</span>
        <div class="fw-bold mt-1">${esc(s.title)}</div>
        ${(s.paragraphs || []).map((p) => `<p>${renderInline(p, numbers)}</p>`).join("")}
        ${nums ? `<div class="chip-row mt-1">${nums}</div>` : ""}
      </div>`;
    })
    .join("");
  return sectionsHtml;
}

function renderInline(text, numbers) {
  let out = esc(text);
  for (const [k, n] of Object.entries(numbers || {})) {
    out = out.replaceAll(
      `{${k}}`,
      `<span class="num-badge"><span class="nv">${esc(fmtNum(n.value))}</span>${
        n.unit ? esc(n.unit) : ""
      }</span>`
    );
  }
  return out;
}

// ── 建议包 ──────────────────────────────────────────────────────────────────

export function recommendationCard(rec) {
  if (!rec?.actions?.length) return "";
  const conf = { high: "bg-success-lt", medium: "bg-info-lt", low: "bg-warning-lt" };
  return rec.actions
    .map(
      (a, i) => `
    <div class="card card-sm mb-2"><div class="card-body py-2">
      <div class="d-flex align-items-center gap-2 mb-1">
        <span class="badge bg-purple-lt">做法 ${i + 1}</span>
        <span class="badge ${conf[a.confidence] || "bg-secondary-lt"}">把握 ${esc(CONF_LABEL[a.confidence] || a.confidence || "?")}</span>
      </div>
      <div class="fw-bold">${esc(a.action)}</div>
      <div class="text-secondary small">${esc(a.rationale || "")}</div>
    </div></div>`
    )
    .join("");
}

// ── 后果推演 / 满意化发现 ──────────────────────────────────────────────────

export function consequenceCard(cc) {
  if (!cc) return "";
  const rows = (cc.reactions || [])
    .map(
      (r) => `
    <div class="card card-sm mb-2"><div class="card-body py-2">
      <div class="d-flex align-items-center gap-2 flex-wrap">
        <span class="badge bg-info-lt">对方可能：${esc(r.reaction)}</span>
        <span class="badge bg-secondary-lt">概率 ${(Number(r.probability) * 100).toFixed(0)}%</span>
        ${r.is_novel ? `<span class="badge bg-warning-lt">新招</span>` : ""}
      </div>
      <div class="chip-row mt-1">
        ${Object.entries(r.outcomes || {})
          .map(
            ([who, o]) =>
              `<span class="param-chip"><span class="pn">${esc(who)}</span><span class="pv">偏低 ${esc(
                fmtNum(o.p10)
              )} · 中间 ${esc(fmtNum(o.p50))} · 偏高 ${esc(fmtNum(o.p90))}</span></span>`
          )
          .join("")}
      </div>
      ${(r.drivers || []).map((d) => `<div class="text-secondary small">· ${esc(d)}</div>`).join("")}
    </div></div>`
    )
    .join("");
  return `<div class="text-secondary small mb-2">${esc(cc.note || "")}</div>${rows}`;
}

export function discoveryCard(dc) {
  const r = dc?.report || {};
  if (!dc) return "";
  const sat = (r.satisfied || [])
    .map(
      (s) => `
    <div class="card card-sm mb-2"><div class="card-body py-2">
      <span class="badge bg-success-lt me-2">够上了</span><b class="mono">${esc(s.strategy || s.id || "?")}</b>
      ${(s.criteria || [])
        .map((c) => `<div class="text-secondary small">· ${esc(typeof c === "string" ? c : JSON.stringify(c))}</div>`)
        .join("")}
    </div></div>`
    )
    .join("");
  const relax = (r.relaxation || [])
    .map(
      (x) =>
        `<div class="text-secondary small">· 放宽“${esc(x.criterion)}”：${esc(fmtNum(x.current))} → ${esc(
          fmtNum(x.suggested)
        )}，约 ${x.candidates_meeting} 个能够上</div>`
    )
    .join("");
  return `
    ${
      r.has_satisfied
        ? `<div class="chip-row mb-2"><span class="badge bg-success-lt">有够上的做法</span></div>${sat}`
        : `<div class="chip-row mb-2"><span class="badge bg-danger-lt">没有够上的做法</span><span class="badge bg-secondary-lt">差一档的备选：${esc(
            (r.pareto_front || []).join("、")
          )}</span></div>`
    }
    ${relax}
    <div class="text-secondary small mt-2">${esc(r.note || dc.note || "")}</div>`;
}