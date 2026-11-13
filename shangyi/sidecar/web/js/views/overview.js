// @ts-check
// 概览：统计瓦片（Tabler stats card）+ 最近案例表 + 预测留档预览。
// 案例行主标题 = 决策问题（与案例库同款），case ID 降为小字。
import { api } from "../api.js";
import { esc, fmtInt, fmtAgo, MODE_LABEL, TERM, humanTitle } from "../util.js";

const MODE_BADGE = { scenario: "bg-success-lt", whatif: "bg-purple-lt", discover: "bg-info-lt" };

export async function render(view, ctx) {
  view.innerHTML = `<div class="empty"><div class="empty-subtitle"><div class="spinner-border spinner-border-sm text-secondary"></div> 加载中…</div></div>`;
  const [casesRes, libRes, health] = await Promise.all([
    api.cases(),
    api.library().catch(() => ({ cases: [] })),
    api.health().catch(() => null),
  ]);
  const cases = casesRes.cases || [];
  const lib = libRes.cases || [];
  const byMode = {};
  for (const c of cases) byMode[c.mode] = (byMode[c.mode] || 0) + 1;
  const recent = cases.slice(0, 8);
  const libRecent = lib.slice(-5).reverse();

  view.innerHTML = `
  <div class="row row-deck row-cards">
    <div class="col-sm-6 col-lg-3">
      <div class="card card-sm">
        <div class="card-body">
          <div class="subheader">案例总数</div>
          <div class="h1 mb-2">${fmtInt(cases.length)}</div>
          <div class="text-secondary">${esc(cases[0]?.date || "")} 最新分片</div>
        </div>
      </div>
    </div>
    <div class="col-sm-6 col-lg-3">
      <div class="card card-sm">
        <div class="card-body">
          <div class="subheader">模式分布</div>
          <div class="h2 mb-2 d-flex flex-wrap gap-1 pt-1">
            ${Object.entries(byMode)
              .map(
                ([m, n]) =>
                  `<span class="badge ${MODE_BADGE[m] || "bg-secondary-lt"}">${esc(
                    MODE_LABEL[m] || m
                  )} ${n}</span>`
              )
              .join("")}
          </div>
        </div>
      </div>
    </div>
    <div class="col-sm-6 col-lg-3">
      <div class="card card-sm">
        <div class="card-body">
          <div class="subheader">预测留档</div>
          <div class="h1 mb-2">${fmtInt(lib.length)}</div>
          <div class="text-secondary">冻结的均衡预测（回测校准用）</div>
        </div>
      </div>
    </div>
    <div class="col-sm-6 col-lg-3">
      <div class="card card-sm">
        <div class="card-body">
          <div class="subheader">求解引擎</div>
          <div class="h2 mb-2 pt-1">${
            health?.shangyi_bin
              ? `<span class="badge bg-success-lt">在线</span>`
              : `<span class="badge bg-warning-lt">未检出</span>`
          }</div>
          <div class="text-secondary text-truncate" title="${esc(health?.shangyi_bin || "")}">${
            health?.shangyi_bin
              ? `<span class="mono" style="font-size:10.5px">${esc(health.shangyi_bin)}</span>`
              : "设 SHANGYI_BIN 后重启 sidecar"
          }</div>
        </div>
      </div>
    </div>
  </div>

  <div class="row row-deck row-cards mt-3">
    <div class="col-lg-7">
      <div class="card">
        <div class="card-header">
          <h3 class="card-title">最近案例</h3>
          <div class="card-actions"><a href="#/cases" class="btn btn-link btn-sm">全部 →</a></div>
        </div>
        <div class="table-responsive">
          <table class="table table-vcenter card-table">
            <thead><tr><th>模式</th><th>案例</th><th>${esc(TERM.playbook)}</th><th class="text-end">事件</th><th class="text-end">最近活动</th></tr></thead>
            <tbody>
            ${
              recent.length
                ? recent
                    .map((c) => {
                      const q = humanTitle(c.question || "");
                      return `
              <tr class="cursor-pointer" onclick="location.hash='#/case/${c.date}/${c.case_id}'">
                <td><span class="badge ${MODE_BADGE[c.mode] || "bg-secondary-lt"}">${esc(MODE_LABEL[c.mode] || c.mode || "?")}</span></td>
                <td style="max-width:420px">
                  <div class="fw-bold text-truncate${q ? "" : " text-secondary"}" title="${esc(q)}">${esc(q || "（未留档决策问题）")}</div>
                  <div class="text-secondary mono" style="font-size:11px">${esc(c.case_id)}</div>
                </td>
                <td class="text-secondary mono" style="font-size:12px">${esc(c.playbook_id || "—")}</td>
                <td class="text-end mono">${c.n_events}</td>
                <td class="text-end text-secondary">${fmtAgo(c.mtime)}</td>
              </tr>`;
                    })
                    .join("")
                : `<tr><td colspan="5"><div class="empty"><p class="empty-title">还没有案例</p><p class="empty-subtitle">去「运行咨询」跑第一个</p></div></td></tr>`
            }
            </tbody>
          </table>
        </div>
      </div>
    </div>
    <div class="col-lg-5">
      <div class="card">
        <div class="card-header">
          <h3 class="card-title">预测留档</h3>
          <div class="card-actions"><span class="text-secondary small">shangyi backtest 的校准样本</span></div>
        </div>
        <div class="card-body pt-2">
        ${
          libRecent.length
            ? libRecent
                .map((c) => {
                  const q = humanTitle(c.decision_question || "");
                  return `
          <div class="card card-sm mb-2"><div class="card-body py-2">
            <div class="d-flex align-items-center gap-2">
              <b class="mono" style="font-size:12px">${esc(c.case_id || "?")}</b>
              <span class="ms-auto badge bg-secondary-lt">${esc(c.prediction?.kind || "预测")}</span>
            </div>
            <div class="text-secondary small text-truncate" title="${esc(q)}">${esc(q || c.scenario || "—")}</div>
          </div></div>`;
                })
                .join("")
            : `<div class="empty"><p class="empty-title">留档为空</p></div>`
        }
        </div>
      </div>
    </div>
  </div>`;
}