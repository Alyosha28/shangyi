// @ts-check
// 工具函数：转义、格式化、有理数。纯函数，无状态。

export function esc(s) {
  return String(s ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

export function fmtNum(n) {
  if (n == null || Number.isNaN(Number(n))) return "—";
  const v = Number(n);
  if (Math.abs(v) >= 1e8) return (v / 1e8).toFixed(2) + "亿";
  if (Math.abs(v) >= 1e4) return (v / 1e4).toFixed(1) + "万";
  if (Number.isInteger(v)) return String(v);
  return v.toFixed(Math.abs(v) < 1 ? 4 : 2).replace(/\.?0+$/, "");
}

export function fmtInt(n) {
  return Number(n ?? 0).toLocaleString("zh-CN");
}

export function fmtBytes(b) {
  if (b == null) return "—";
  if (b >= 1048576) return (b / 1048576).toFixed(1) + " MB";
  if (b >= 1024) return (b / 1024).toFixed(0) + " KB";
  return b + " B";
}

export function fmtTime(iso) {
  if (!iso) return "—";
  return iso.replace("T", " ").replace("Z", " UTC");
}

export function fmtAgo(unixSec) {
  if (!unixSec) return "—";
  const d = Date.now() / 1000 - unixSec;
  if (d < 90) return "刚刚";
  if (d < 3600) return Math.round(d / 60) + " 分钟前";
  if (d < 86400) return Math.round(d / 3600) + " 小时前";
  return Math.round(d / 86400) + " 天前";
}

/** 精确有理数（引擎输出形如 "4/5"、"0"、"380000"、"-3/2"）→ {frac, float}。 */
export function parseRational(s) {
  const t = String(s ?? "").trim();
  const m = t.match(/^(-?\d+)\/(-?\d+)$/);
  if (m) {
    const a = Number(m[1]), b = Number(m[2]);
    return { frac: t, float: b === 0 ? NaN : a / b };
  }
  const v = Number(t);
  return { frac: t, float: v };
}

/** 求解方法 / 精炼方法的中文短名（展示层文案，不改变语义）。 */
export const METHOD_LABEL = {
  support_enumeration: "挨个试（最全）",
  lemke_howson: "Lemke-Howson",
  analytic: "直接算",
};
export const REFINE_LABEL = {
  trembling_hand_perfect: "手抖也不变",
  ess_symmetric: "长期稳定·同类",
  ess_two_population: "长期稳定·两类",
};

/** 界面术语映射：内部标识 → 用户语言。唯一出处——视图一律从这里取词，
 *  不许在视图里直写「剧本 / intake / 归档」这类内部黑话。 */
export const TERM = {
  playbook: "分析方案", // 内部叫「剧本」（playbook.yml）
  intake: "背景问答", // 阶段 intake（原「受理」）
  archived: "已完成", // 阶段 archived（原「归档」）
};

export const PHASE_LABEL = {
  intake: TERM.intake,
  intel: "摸底",
  model_build: "摆局",
  solve: "算稳的",
  select: "挑一个",
  consequence: "推下去",
  discover: "找好的",
  recommend: "建议",
  report: "报告",
  archived: TERM.archived,
};

export const MODE_LABEL = { scenario: "看可能", whatif: "试做法", discover: "找做法" };

/** 费用整理的口径说明：**这次只算用户列出的项目**，别把范围外的缺口说成"没算完"。
 *
 *  UI-013：原来只要 status 是 partial 就一律显示「交了能交的」+「这些还没问到」，
 *  于是用户明明已经算清自己指定的那几项，看到的还是"部分交付"和一堆范围外的缺口。
 *  后端其实分开记了三件事：
 *    - amounts_complete          列进来的项目金额是否都取得了
 *    - cash_classification_complete 列进来的项目性质是否都分类了
 *    - costs_complete            模型**显式声明**这份清单是否完整（未列费用不当零）
 *  前两项说的是"你要的算清没"，第三项说的是"清单是否声明完整"——不能混为一谈。
 */
export function costScopeNote(result) {
  const scenario = (result?.cost_scenarios || []).find((c) => c && typeof c === 'object');
  if (!scenario) return '';
  const listed = [];
  if (scenario.listed_monthly_fixed_cost != null) listed.push('每月固定 ' + scenario.listed_monthly_fixed_cost);
  if (scenario.listed_upfront_cash != null) listed.push('一次性现金 ' + scenario.listed_upfront_cash);
  const parts = [];
  if (scenario.amounts_complete === true && scenario.cash_classification_complete === true) {
    parts.push('你列的项目都算清了' + (listed.length ? '（' + listed.join('、') + '）' : '') + '。');
  } else {
    // 列进来的项目里还有没算清的：点名是哪些，别只说"部分"
    const missingAmount = (scenario.missing_amount_items || []).map((x) => x?.name).filter(Boolean);
    const unclassified = (scenario.unclassified_items || []).map((x) => x?.name).filter(Boolean);
    const named = [...new Set([...missingAmount, ...unclassified])];
    parts.push('你列的项目里还有 ' + (named.length ? named.join('、') : '几项') + '没算清（金额没取得或性质待定）。');
  }
  if (scenario.costs_complete === false) {
    parts.push('这份清单没有声明完整：没列进来的费用不在这次范围内，也不当成零。要更全的账，把项目补上再算。');
  }
  return parts.join('');
}



/** 「这次做了什么」——给普通人看的任务名。
 *
 *  UI-014：列表原来只按内部分析模式（scenario/whatif/discover）打标签，
 *  一次「帮我算算费用」的记录于是显示成「情景分析」，用户得先看懂内部模式
 *  才能认出自己刚提交的是什么。这里把任务名统一到一处，列表、对话、详情
 *  三个界面用同一套词。
 *
 *  与 MODE_LABEL 的关系：mode 是**分析路径**（ BRIEF 任务不走它），
 *  task_scope 是**用户要求做的事**。前者保留给懂行的人看，后者是列表的标签。 */
export const TASK_LABEL = {
  consultation: "经营分析",
  research: "资料整理",
  cost: "费用整理",
  research_and_cost: "资料和费用",
  model: "数字摆局",
};

/** 列表筛选用：与上面的标签一一对应，顺序即按钮顺序。 */
export const TASK_FILTERS = [
  ["all", "全部"],
  ["consultation", TASK_LABEL.consultation],
  ["research", TASK_LABEL.research],
  ["cost", TASK_LABEL.cost],
  ["research_and_cost", TASK_LABEL.research_and_cost],
  ["model", TASK_LABEL.model],
];

/** 归档案例、对话、摆局分别属于哪类「做的事」。 */
export function taskKindOf(record) {
  if (record?.task_scope && TASK_LABEL[record.task_scope]) return record.task_scope;
  if (record?.kind === "model") return "model";
  return "consultation";
}

/** 置信度中文短名（recommendation.actions[].confidence）。 */
export const CONF_LABEL = { high: "高", medium: "中", low: "低" };

/** 决策问题 / 结论 headline → 人话标题：取首个非空行，去 markdown 强调
 *  与「决策问题：」「【决策问题】」「[补档标记]」类前缀。留档原文可能是
 *  多段长文，列表只放标题。 */
export function humanTitle(s) {
  const line = (String(s ?? "").split("\n").find((l) => l.trim()) || "").trim();
  return line
    .replace(/\*+/g, "")
    .replace(/^\[[^\]]*]\s*/, "")
    .replace(/^【决策问题】\s*/, "")
    .replace(/^决策问题[：:]\s*/, "")
    .trim();
}

/** 各模式的阶段路径（与 shangyi-core PhaseGraph::for_mode 一致的静态镜像，仅用于步进条展示）。 */
export const MODE_PHASES = {
  scenario: ["intake", "intel", "model_build", "solve", "select", "recommend", "report", "archived"],
  whatif: ["intake", "intel", "model_build", "solve", "consequence", "recommend", "report", "archived"],
  discover: ["intake", "intel", "model_build", "solve", "discover", "recommend", "report", "archived"],
};

/** 检查点落在哪个阶段之后（步进条上的 ◆ 标记）。 */
export const CHECKPOINT_AT = { cp1: "intel", cp2: "model_build", cp3: null }; // cp3 按模式落在 select/consequence/discover

export function phaseLabel(p) {
  return PHASE_LABEL[p] || p;
}

/** 把 GameSpec 的 Expr（literal/ref）解析成数值；ref 走 params 表。 */
export function exprValue(expr, params) {
  if (expr == null) return null;
  if (expr.kind === "literal") return Number(expr.value);
  if (expr.kind === "ref") {
    const p = params?.[expr.value];
    if (p && p.value != null) {
      if (typeof p.value === "number") return p.value;
      if (p.value?.kind === "literal") return Number(p.value.value);
    }
  }
  return null;
}