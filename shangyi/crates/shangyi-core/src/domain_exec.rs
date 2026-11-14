//! 领域工具执行器：把 [`ToolRegistry`] 的 wire 名接到真实引擎（设计文档 §7.2 的工具表）。
//!
//! # 三条纪律
//!
//! 1. **铁律 1：数字只出自确定性引擎。** 本模块的每个数值都来自
//!    `shangyi-solver` / `shangyi-selection` / `shangyi-consequence` 等纯 Rust 代码。
//!    本模块**不做**任何近似、插值或"模型估计"——它只调用与序列化。
//! 2. **能力 seam 失败即拒绝，绝不静默降级**（不变量②）。遇到不支持的输入，
//!    返回类型化错误，**不**退回到一个"差不多的"计算。
//! 3. **"无法评估" ≠ "评估为负面"**（[`ExecOutcome::Blocked`]）。需要外部网络而
//!    适配器缺失时，必须报 `Blocked`——报告里那叫"未取得"，不是"没有"。
//!
//! # 当前接线状态（诚实清单）
//!
//! | 工具 | 状态 |
//! |---|---|
//! | `gamespec_build` / `gamespec_validate` | ✅ 接 `shangyi-gamespec` |
//! | `solve_equilibria` | ✅ 接 `shangyi-solver`（`NormalFormGame::from_gamespec`） |
//! | `consequence_predict` | ✅ 条件子博弈 + 对手最佳反应（**whatif 的核心**） |
//! | `monte_carlo` | ✅ 接 `shangyi-consequence`（种子固定） |
//! | `sensitivity_scan` | ✅ 接 `shangyi-consequence`（翻转阈值） |
//! | `novel_move_filter` | ✅ 接 `shangyi-consequence::TripleFilter` |
//! | `discover_satisfice` | ✅ 接 `shangyi-discover` |
//! | `kb_retrieve` | ✅ 接 `shangyi-kb` 理论库 |
//! | `pars_check` | ✅ PARTS 完备性检查（**材料够不够**，不是模型选择） |
//! | `case_store_read` / `case_store_write` | ✅ 接 `shangyi-kb` 案例库（归档时**冻结预测**） |
//! | `report_render` | ✅ 产出报告模型（每个数字带 provenance；渲染归侧车） |
//! | `vn_map` | ✅ 接 `shangyi-playbooks` 的价值网声明（产出**扫描清单与覆盖**，不发现参与者） |
//! | `ask_user` | ✅ 从剧本 intake 清单提问（用户输入 seam） |
//! | `checkpoint` | ✅ 向用户确认本阶段事实/假设/结果（**铁律 2 的落点**） |
//! | `request_rewind` | ✅ 模型**提议**回退（决定权在用户，§7.5） |
//! | `web_fetch` | ✅ 真实抓取（`shangyi-intel::fetch`：robots 遵从 + 限速 + 反爬识别）；未绑定 provider 时 `Blocked` |
//! | `web_search` | ⚠️ **`Blocked`**——需配置检索后端（API key 或自托管实例） |
//! | `participant_profile_update` | ✅ 接 `shangyi-intel` 贝叶斯推断 |
//! | `select_equilibria` / `select_fuse` | ✅ 接 `shangyi-selection` |
//! | `preference_inject` | ✅ 把用户偏好映射为选择器权重（**真正影响推荐**） |
//! | `recommend_build` | ✅ 汇总建议包（**缺它则 report 阶段结构上不可达**） |
//! | 其余（`data_ingest`/`swarm_run`/`sandbox_exec`） | ⛔ 未接线，返回类型化 `Failed` |
//!
//! 未接线的工具**返回 `Failed` 而非 `Blocked`**：它们的缺口是"实现未完成"，
//! 不是"外部依赖不可用"，两者混同会让审计以为外部环境有问题。

use std::collections::BTreeMap;

use shangyi_gamespec::{GameSpec, ValidationError};
use shangyi_intel::{ParticipantProfile, SourceKind};
use crate::sandbox as shangyi_core_sandbox;
use shangyi_kb::TheoryLibrary;
use shangyi_playbooks::Playbook;
use shangyi_selection::{
    EquilibriumDistribution, SelectorId, SelectorScore, SelectorWeights,
};
use shangyi_solver::{NormalFormGame, SolveRequest, SolverBackend};

use crate::agent::{ExecOutcome, ToolCall, ToolExecutor, UserQuestion};
use crate::consultation::ConsultationMode;
use crate::phase_graph::{ArtifactKind, SessionPhase};
use crate::task_scope::{ScopedTaskResult, TaskResultStatus, TaskScope};
use crate::constraints::{Acceptance, ConstraintAttribute, ConstraintBook, UserInputFact};
use crate::conditional::{ConditionalSnapshot, ConditionalTrigger, EvidenceGap, EvidenceGapReason};

fn mentions_unknown_payoff(text:&str)->bool {
    ["利润","收益","收入","效用","支付"].iter().any(|word|text.contains(word))
        && ["未知","不知道","不清楚","未取得","不掌握","不确定"].iter().any(|word|text.contains(word))
}

/// 外部检索的 seam（`web_search` / `web_fetch` 的落点）。
///
/// # 为什么它现在没有实现
///
/// 公开数据检索需要一个真实的后端（搜索 API、抓取器、robots.txt 与限速策略）。
/// 在**没有**适配器时，正确行为是 [`ExecOutcome::Blocked`]——"无法评估"——
/// 而不是编造一条情报。这与 §7.1 的 `Blocked` 语义严丝合缝：一次因数据源
/// 不可用而中断的尽调，必须与一次正常得出结论的尽调**永久可区分**。
///
/// 提供一个 trait（而不是直接 `unimplemented!()`）的理由：它使"接线后行为"
/// 可在测试里用假 provider 穷举验证，而不必等真实后端就位。
pub trait SearchProvider {
    /// 检索。返回命中的公开页面。
    fn search(&self, query: &str) -> Result<Vec<SearchHit>, SearchUnavailable>;
    fn search_with_health(&self, query: &str) -> Result<(Vec<SearchHit>, shangyi_intel::fetch::SearchHealth), SearchUnavailable> {
        self.search(query).map(|hits| {
            let health = shangyi_intel::fetch::SearchHealth::unreported(hits.len());
            (hits, health)
        })
    }
    /// 抓取正文。
    fn fetch(&self, url: &str) -> Result<FetchedText, SearchUnavailable>;
}

/// 为什么检索/抓取没有结果。
///
/// # 三态（而不是 `Result<_, String>`）
///
/// | 变体 | 含义 | 报告里读作 |
/// |---|---|---|
/// | （`Ok(空)`） | 查询没有结果 | **没有** |
/// | `NoBackend` | 没配后端 / 未接线 | **未取得** |
/// | `Failed` | 后端出错 | **出错** |
///
/// 早先的 `Result<_, String>` 把后两者混成一个字符串，于是 `domain_exec` 只能
/// 一律当 `Blocked`——"后端 401 未授权"（该去修配置）与"没配后端"（该去申请 key）
/// 在报告里长得一样。三态让它们分开。
#[derive(Debug, Clone, PartialEq)]
pub enum SearchUnavailable {
    /// 未配置后端。**这是"未取得"，不是"没有"。**
    NoBackend { detail: String },
    /// 后端存在但失败（网络、鉴权、限流、被 robots 禁止…）。
    Failed {
        detail: String,
        /// **重试有无意义**。
        ///
        /// # 为什么这个字段必须存在（实测：同一个 404 URL 被试了 4 次）
        ///
        /// run24 里模型对同一批 URL 连续重试。它的行为在信息不足时是**理性的**
        /// ——"抓取失败"没有回答它真正的问题：换个时间再试有用吗？
        ///
        /// 而两类失败的处置方向**完全相反**：
        ///
        /// | 失败 | 重试 | 正确动作 |
        /// |---|---|---|
        /// | 404 / 证书过期 / 人机验证 / robots 明确禁止 | ❌ | 换数据源 |
        /// | 429 / 5xx / 网络超时 / robots 获取失败 | ✅ | 有限重试；未获许可前不抓正文 |
        ///
        /// 把"重试无用"说得像"暂时失败"，就是在鼓励浪费 turn——而 turn 是
        /// 真实成本。分类由 `FetchError::retryable()` 在类型层面给出
        /// （那里有完整的错误变体），不在文本上匹配状态码。
        retryable: bool,
    },
}

impl std::fmt::Display for SearchUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchUnavailable::NoBackend { detail } => write!(f, "{detail}"),
            SearchUnavailable::Failed { detail, .. } => write!(f, "{detail}"),
        }
    }
}

/// 周边门店事实的 seam（`poi_survey` 的落点）。
///
/// # 为什么单独一个 trait，而不是塞进 [`SearchProvider`]
///
/// 网页检索回答「有没有公开资料这么说」，周边计数回答「这个半径内实际有几家
/// 同类门店」——两者的**证据等级不同**（POI 是坐标级计数，不是文章转述），
/// 失败方式也不同（限流、地址解析失败）。混成一个 trait 会让"检索失败"与
/// "计数未取得"在报告里长成一样，而它们的处置方向完全不同。
///
/// # 只回计数，不回明细
///
/// 工具输出会进事件日志（长期保存）。地图服务条款限制 POI 明细的长期存储与
/// 再分发，所以这一层**只回计数**：门店名不在引擎里落盘。
pub trait PoiProvider {
    fn survey(&self, query: &PoiQuery) -> Result<PoiSurvey, SearchUnavailable>;
}

/// 一次周边查询的输入。
#[derive(Debug, Clone, PartialEq)]
pub struct PoiQuery {
    pub address: String,
    /// 半径（米）。提供方负责钳制到合规范围。
    pub radius: u32,
    /// 同品牌核查（如「喜茶」）：这个品牌在该半径内已经开了几家。
    pub brand: String,
    /// 额外关键词计数（如「宠物店」）；空表示只查默认类目。
    pub keywords: String,
}

/// 一类周边设施的数量。
#[derive(Debug, Clone, PartialEq)]
pub struct PoiCount {
    /// 类目名（如「茶饮咖啡」「写字楼」）。
    pub label: String,
    pub count: u64,
}

/// 周边计数结果。**不含门店明细**——见 [`PoiProvider`] 的纪律说明。
#[derive(Debug, Clone, PartialEq)]
pub struct PoiSurvey {
    pub address: String,
    pub district: String,
    pub radius: u32,
    pub counts: Vec<PoiCount>,
    /// 同品牌存量（未核查时为 `None`，不是 0）。
    pub brand: Option<PoiCount>,
    /// 额外关键词计数（未查询时为 `None`）。
    pub keywords: Option<PoiCount>,
    /// 逐项没查到的原因（限流等）。**未取得不是 0**。
    pub unavailable: Vec<String>,
    /// 数据来源标识（进报告供审计）。
    pub source: String,
}

impl SearchUnavailable {
    /// 给**模型**看的完整说明（含可操作的下一步）。
    ///
    /// 与 `Display` 分开是刻意的：日志里要简洁，而给模型的文本必须
    /// 回答"我该怎么办"——否则它会自己猜一个动作（通常是再试一次）。
    pub fn actionable(&self) -> String {
        match self {
            SearchUnavailable::NoBackend { detail } => detail.clone(),
            SearchUnavailable::Failed { detail, retryable } => {
                if *retryable {
                    format!(
                        "{detail}\n→ 这可能是**临时性故障**，稍后重试可能成功；\
                         但若连续失败，请改换数据源（不要在同一 URL 上循环）。"
                    )
                } else {
                    format!(
                        "{detail}\n→ **重试无用**：该资源不存在、永久不可达，\
                         或站点明确拒绝抓取。请**换数据源**——\
                         同一请求再试只会得到同样的结果。"
                    )
                }
            }
        }
    }
}

/// 抓取到的正文（带来源信息，便于留 provenance）。
#[derive(Debug, Clone, PartialEq)]
pub struct FetchedText {
    pub url: String,
    pub text: String,
    pub raw_bytes: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
    /// 哪个后端给出的。多后端融合去重后为 `+` 连接的全部来源
    /// （如 `bocha+tavily`）——一条文档被多个后端同时返回是共识证据。
    pub backend: String,
    /// 供应商返回的页面日期元数据。**未经原文核对**，只能作线索；
    /// 报告引用日期仍须来自 `web_fetch` 的正文。
    pub published_at: Option<String>,
    /// 融合排序分（0–100）。它只表示检索排序，不表示事实真伪。
    pub rank_score: u32,
    /// 命中的排序信号（中文短标签，如"官方域名""年份相符"）。
    pub rank_signals: Vec<String>,
}

/// Consecutive unsuccessful provider calls for one URL in the current task.
#[derive(Debug, Clone, Copy)]
struct FetchFailure {
    attempts: u8,
    retryable: bool,
}

/// 领域执行器的会话内状态。
///
/// **它不持有 [`crate::PhaseMachine`]**：阶段机是闸门的唯一权威，执行器是引擎
/// 产物的唯一权威。两者由循环协调（见 [`ToolExecutor::artifacts`]）。
#[derive(Debug, Default)]
pub struct DomainState {
    pub constraints: ConstraintBook,
    constraint_invalidations: Vec<ArtifactKind>,
    pub task_scope: Option<TaskScope>,
    pub task_request_text: String,
    pub research_task: Option<crate::research_task::ResearchTask>,
    pub attempted_model: Option<serde_json::Value>,
    pub game_spec_evidence: Option<serde_json::Value>,
    pub decision_gaps: Vec<crate::conditional::EvidenceGap>,
    task_retrieval_attempts: usize,
    task_sources: std::collections::BTreeSet<String>,
    task_costs: BTreeMap<String, serde_json::Value>,
    task_gaps: Vec<String>,
    pub fetched_sources: std::cell::RefCell<BTreeMap<String, String>>,
    pub fetched_pages: std::cell::RefCell<BTreeMap<String, serde_json::Value>>,
    fetch_failures: std::cell::RefCell<BTreeMap<String, FetchFailure>>,
    pub research_records: Vec<serde_json::Value>,
    /// 本次会话内工具产生的结构化查询回执（编号 → 回执）。
    ///
    /// 只有工具能写入；`research_task_update` 只读它来校验模型引用的编号，
    /// 从而让「结构化计数」无法被模型自述伪造。
    pub structured_receipts: crate::research_task::StructuredReceipts,
    pub cost_scenarios: BTreeMap<String, serde_json::Value>,
    pub asked_question_texts: std::collections::BTreeSet<String>,
    pub retrieval_attempts: std::cell::Cell<usize>,
    pub profiles: Vec<ParticipantProfile>,
    pub game_spec: Option<GameSpec>,
    pub equilibria_json: Option<String>,
    pub distribution: Option<EquilibriumDistribution>,
    pub report_json: Option<String>,
    /// 已采纳的判据权重（`select_fuse` 用；未校准时为等权基线）。
    pub selector_weights: Option<SelectorWeights>,
    /// 本轮已评估的选择器分数（结算 select 阶段用）。
    pub selector_scores: BTreeMap<SelectorId, Vec<SelectorScore>>,
    /// 已回答过的 intake 问题 id（`ask_user` 的 `skip_answered` 用）。
    pub answered_intake: std::collections::BTreeSet<String>,
    /// 案例库（`case_store_*`）。冷启动期为空——**空库不等于"没有可用先验"**，
    /// 而是在报告里如实标注"未校准"（§12 风险表）。
    pub case_library: shangyi_kb::CaseLibrary,
    /// 案例库的**持久化路径**。`None` = 不落盘（测试/只读场景）。
    ///
    /// # 为什么它必须在 state 里（实测审计发现的根本缺口）
    ///
    /// `CaseLibrary` 此前是**纯内存**的：`case_store_write` 归档的案例
    /// 进程退出即丢 ⇒ 案例库**永远积累不到**校准所需的 30 个样本
    /// ⇒ `calibration_ready` 永远为 false ⇒ 报告里"权重待校准"
    /// 那句话永远不变。
    ///
    /// 由 CLI 组装层注入（它知道 `$SHANGYI_HOME`），执行器只负责用。
    pub case_library_path: Option<std::path::PathBuf>,
    /// 已产出的报告。
    pub report_model: Option<shangyi_report::Report>,
    /// 条件后果报告（whatif 模式的核心产物）。
    pub consequence_report: Option<shangyi_consequence::ConsequenceReport>,
    /// 建议包（recommend 阶段的交付物）。缺它则 report 阶段不可达。
    pub recommendation: Option<String>,
    /// 满意策略发现报告（discover 阶段的交付物）。
    ///
    /// 与 `consequence_report` 同样：它是**阶段图要求的产物**，
    /// 缺登记则 `discover` → `recommend` 不可达。
    pub discovery_report: Option<shangyi_discover::DiscoveryReport>,
    /// `swarm_run` 的产物：persona rollout 的行动计数与候选评估。
    ///
    /// 存下来是为了让 `report` 阶段能引用它（它是 `persona_sim` 判据的
    /// **行为证据**，而不是结论本身——见 crate 文档的「提议 ≠ 结论」）。
    pub swarm_output: Option<serde_json::Value>,
    /// `data_ingest` 已解析的文件报告（用户提供的数据）。
    ///
    /// 存下来是为了让报告能引用"用户给了什么数据"——而**不**是把
    /// 侧车的输出当引擎结果用（可信度分级不同，见 `TrustLevel::Sidecar`）。
    pub ingested: Vec<shangyi_intel::sidecar::IngestReport>,
    /// LLM-Wiki 知识库（R22）。`None` = 未加载。
    ///
    /// **未加载与空库必须区分**：前者是「没接上」，后者是「接了但没有内容」。
    /// 混同会让「知识库没配」表现为「知识库里没有这条理论」。
    pub wiki: Option<std::sync::Arc<shangyi_kb::wiki::Wiki>>,
}

/// **建议依据**——`recommend_build` 接受哪一类引擎产出作为支撑。
///
/// 见 `recommend_build` 的注释：依据按模式而异（scenario 用均衡分布、
/// whatif 用后果报告、discover 用满意策略评估），但共同点必须是
/// **引擎算出来的**——那才是这条前置检查的目的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecommendationBasis {
    EquilibriumDistribution,
    ConsequenceReport,
    DiscoveryReport,
}

impl RecommendationBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            RecommendationBasis::EquilibriumDistribution => "equilibrium_distribution",
            RecommendationBasis::ConsequenceReport => "consequence_report",
            RecommendationBasis::DiscoveryReport => "discovery_report",
        }
    }
}

/// 领域工具执行器。
pub struct DomainExecutor<'a> {
    state: DomainState,
    theory: &'a TheoryLibrary,
    playbook: &'a Playbook,
    search: Option<&'a dyn SearchProvider>,
    /// 周边事实的数据源。未接线 ⇒ `poi_survey` 返回 `Blocked`（未取得）。
    poi: Option<&'a dyn PoiProvider>,
    /// 咨询模式。
    ///
    /// # 为什么执行器需要它（而不是"执行器不该知道阶段机"）
    ///
    /// 早先我把"执行器不该知道阶段机"当成一条设计原则，于是 `checkpoint` 工具
    /// **没有被实现**——因为"该打开哪个 checkpoint"看起来只有阶段机知道。
    /// 后果是铁律 2 在真实运行中从未生效，且流程死锁（见 tests/domain_exec.rs
    /// 的 `checkpoint_tool_must_open_a_checkpoint_not_report_unimplemented`）。
    ///
    /// 正确的划界不是"执行器不知道阶段"，而是**"阶段与 checkpoint 的对应关系
    /// 只有一处定义"**——那就是 [`PhaseGraph`]。执行器持有模式、向它查询映射，
    /// 而不是自己写一份 `if phase == Intel { Cp1 }`。后者才是要避免的重复。
    mode: ConsultationMode,
    /// persona actor 的来源（`swarm_run` 用）。
    ///
    /// 存的是**闭包**而不是 `LlmClient`：这样 `shangyi-core` 不必知道
    /// provider、模型名、温度等接线细节——那些属于 CLI 的组装层。
    /// 返回 `None` 表示"没有可用的 LLM"，此时 `swarm_run` 返回 `Blocked`
    /// （而不是编造一批行动）。
    swarm_actor: Option<Box<dyn SwarmActorFactory<'a> + 'a>>,
}

/// 渲染翻转扫描的结论（两个入口共用：外部点列与引擎自扫描）。
///
/// 抽出来是为了让**两种来源的结论措辞一致**——若各写各的，
/// 同一形状的结果会因措辞不同而被读成不同的东西。
fn render_flip_scan(points: &[(f64, bool)], mode: &str) -> String {
    // 找第一个翻转点，报告它与**前一点**之间为翻转区间。
    //
    // ⚠️ 两个边界都必须处理（实测 panic 暴露）：
    //
    // 1. **第一个点就翻转**（`i == 0`）：没有"前一点"。
    //    早先写 `points[i - 1]`，而 `i: usize` 的减法在 0 处**下溢 panic**
    //    （实测：一次真实咨询在 recommend 阶段因此崩溃）。
    //    此时如实报告"扫描起点之前就已翻转"——**不编一个区间**。
    // 2. 前一点本身就是翻转（连续两点都翻）：说明翻转发生在更早处，
    //    区间应取更靠前的边界。此处按 `find` 的语义取第一个，故不会遇到。
    let flip = points
        .iter()
        .enumerate()
        .find(|(_, (_, f))| *f)
        .map(|(i, (x, _))| {
            if i == 0 {
                return (None, *x);
            }
            (Some(points[i - 1].0), *x)
        });
    let body = match flip {
        // 起点之前就已翻转：**如实说明无从给出下界**，不编一个区间
        Some((None, hi)) => serde_json::json!({
            "flip_interval": [serde_json::Value::Null, hi],
            "note": "扫描的**第一个点就已翻转**，故无法给出下界——\
                     请把扫描范围往更小的方向扩展。这不是「阈值很小」，\
                     而是「当前扫描没有覆盖到翻转前的区间」。",
        }),
        Some((Some(lo), hi)) => serde_json::json!({
            "flip_interval": [lo, hi],
            "note": "翻转区间为相邻两扫描点的夹逼；若需更精确阈值请加密扫描。",
        }),
        None => serde_json::json!({
            "flip_interval": null,
            "note": "在扫描范围内未观察到翻转——这是**范围内**的结论，不等于该结论稳健。",
        }),
    };
    let mut v = body;
    if let Some(o) = v.as_object_mut() {
        o.insert("mode".into(), serde_json::json!(mode));
        o.insert("scan_points".into(), serde_json::json!(points.len()));
        o.insert(
            "flipped_points".into(),
            serde_json::json!(points.iter().filter(|(_, f)| *f).count()),
        );
    }
    serde_json::to_string_pretty(&v).unwrap_or_default()
}

/// 某参数取值下**均衡集合的签名**（用于判定"翻转"）。
///
/// # 签名怎么算
///
/// 把均衡按焦点支付排序，取 (均衡个数, 有序的支付序列) 作为签名。
/// 代价是"签名相同不代表策略相同"（可能只是支付数值巧合）——
/// 该局限在输出里如实标注，并同时给出每点的均衡数与焦点支付供人工核对。
///
/// # 为什么不用"某个均衡的支付"作判据
///
/// 那需要先**选一个均衡**，而选择是 select 阶段的职责（六判据加权融合）。
/// 在这里选一个等于擅自替用户做了选择——而且选法不透明。
///
/// `x` 为 `NaN` 时表示"取参数当前值"（基准点）。
fn equilibrium_signature(
    spec: &shangyi_gamespec::GameSpec,
    param_name: &str,
    x: f64,
) -> Result<String, String> {
    let mut s = spec.clone();
    if !x.is_nan() {
        if let Some(p) = s.params.get_mut(param_name) {
            p.value = shangyi_gamespec::Expr::Literal(x);
            p.dist = None;
        }
    }
    let game = NormalFormGame::from_gamespec(&s)
        .map_err(|e| format!("参数 {param_name}={x} 时博弈无法适配：{e}"))?;
    let sol = shangyi_solver::NativeSolver
        .solve(SolveRequest::new(game))
        .map_err(|e| format!("参数 {param_name}={x} 时求解被拒：{e}"))?;
    let focal = focal_player_index(&s);
    let mut payoffs: Vec<String> = sol
        .equilibria
        .iter()
        .map(|e| format!("{:.6}", e.payoffs[focal].to_f64()))
        .collect();
    payoffs.sort();
    Ok(format!("n={};{}", payoffs.len(), payoffs.join(",")))
}

/// 诊断一段**不平衡**的 JSON：缺几个括号、最后未闭合的在哪。
///
/// # 为什么值得写（实测：同一形状失败 4 次）
///
/// run25 里模型四次把 GameSpec 写成**双重编码**（JSON 放在字符串里）且内层
/// **括号不平衡**。它每次拿到的错误是：
///
/// ```text
/// 参数是字符串，但内容不是合法 JSON：EOF while parsing an object at line 1 column 3473
/// ```
///
/// 这条信息只说了"有问题"，没说**缺什么**。而在 2547 个字符里数括号，
/// 对人类与对模型都不可行 ⇒ 它只能重写一遍，很可能再错。
///
/// 本函数做**确定性的平衡分析**：数清未闭合的圆括号/方括号，并给出最后
/// 未闭合者的位置。这是解析，不是猜测——它**不补全 JSON**（补全等于编造数据），
/// 只报告缺口。
///
/// # 已知边界（如实标注）
///
/// 它按字符扫，**不识别字符串字面量里的括号**（如 `"注释: }"` 里的 `}`）。
/// 那会让计数偏差。故输出里附上位置供人工核对，并说明这一局限。
fn diagnose_unbalanced_json(s: &str) -> String {
    let mut braces = 0i32;
    let mut brackets = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut last_open: Option<(usize, char)> = None;

    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' if !in_string => {
                braces += 1;
                last_open = Some((i, '{'));
            }
            '}' if !in_string => braces -= 1,
            '[' if !in_string => {
                brackets += 1;
                last_open = Some((i, '['));
            }
            ']' if !in_string => brackets -= 1,
            _ => {}
        }
    }

    let mut out = String::new();
    if braces > 0 || brackets > 0 {
        out.push_str(&format!(
            "\n\n**诊断**：内容**不完整**——还缺 {braces} 个 `}}` 与 {brackets} 个 `]`。\n"
        ));
        if let Some((pos, ch)) = last_open {
            out.push_str(&format!(
                "最后未闭合的 `{ch}` 在第 {pos} 个字符处（若那里本应是个字符串，\
                 则本诊断的计数可能有偏差——它不识别字符串里的括号）。\n"
            ));
        }
        out.push_str(
            "→ **建议**：不要直接重写整段（通常仍会错在同一处）。改为**分两步**：\
             先建一个**最小可校验骨架**（`players` + `strategies` + 至少覆盖策略组合的初始字面量 `payoffs.values.cells` + 空 `params: {}`），\
             校验通过后**再带 `merge: true` 调一次 `gamespec_build`** 补参数与 `ref` 引用\
             （`merge: true` 会深合并进已有的 spec，且 `cells` 首次建立时不能为空 `{}`）。\
             长嵌套 JSON 一次写对的概率随长度下降，分两次写更稳。",
        );
    } else if braces < 0 || brackets < 0 {
        out.push_str(&format!(
            "\n\n**诊断**：内容里**多出**了 {} 个 `}}` / {} 个 `]`（闭合多于开启）。\n",
            -braces.min(0),
            -brackets.min(0)
        ));
    } else {
        out.push_str(
            "\n\n**诊断**：括号平衡，说明问题在别处（缺逗号、值为空、键重复等）——\
             请对照上面的解析器位置检查。",
        );
    }
    out
}

/// 焦点玩家（本企业）在 `players` 里的下标。找不到 `focal` 时取 0。
///
/// # 为什么找不到时取 0 而不是报错
///
/// "谁是焦点"在 `GameSpec` 里由 `vn_role == Focal` 声明，但并非所有
/// 建模都会标注它（有些只有一个玩家，或角色标成了 `substitutor`）。
/// 报错会让一个可用的博弈无法做敏感性分析——那阻止的是分析，不是错误。
/// 取 0（确定性）并在输出的 `_caveats` 里如实说明更合理。
fn focal_player_index(spec: &shangyi_gamespec::GameSpec) -> usize {
    spec.players
        .iter()
        .position(|p| p.vn_role == shangyi_gamespec::ValueNetRole::Focal)
        .unwrap_or(0)
}

/// 从已声明的分布里抽一个点（种子由调用方管理 ⇒ 可复现）。
///
/// 三种分布都有明确支撑与参数，**不做隐含截断**：
/// 正态分布理论上可取任意实数，但支付矩阵里的负成本/负概率会失去意义。
/// 故对正态**在 ±4σ 处截断**并如实标注——这是刻画"工程上不会去那么远"，
/// 而不是数学上的正确性主张。均匀与三角分布本身有界，不需处理。
fn sample_from_dist(rng: &mut shangyi_consequence::Lcg, dist: &shangyi_gamespec::Dist) -> f64 {
    use shangyi_gamespec::Dist;
    match dist {
        Dist::Uniform { low, high } => low + (high - low) * rng.next_f64(),
        Dist::Normal { mu, sigma } => {
            let z = rng.next_normal().clamp(-4.0, 4.0);
            mu + sigma * z
        }
        Dist::Triangular { low, mode, high } => {
            // 逆变换法（标准做法，确定性）
            let u = rng.next_f64();
            let (lo, md, hi) = (*low, *mode, *high);
            if (hi - lo).abs() < f64::EPSILON {
                return lo;
            }
            let fc = (md - lo) / (hi - lo);
            if u < fc {
                lo + (u * (hi - lo) * (md - lo)).sqrt()
            } else {
                hi - ((1.0 - u) * (hi - lo) * (hi - md)).sqrt()
            }
        }
    }
}

/// 把模型给的 GameSpec 输入**归一化**成 `GameSpec` 期望的形态。
///
/// # 它做的三件事，以及为什么每一件都值得做（全部来自实测）
///
/// ## 1. 裸数字 → `{"kind":"literal","value":N}`
///
/// `Expr` 是邻接标记枚举（原因见 `shangyi-gamespec` 的序列化规则：
/// `serde_json/arbitrary_precision` 下内部/无标记枚举在数字上会炸。该 feature
/// 现只在**测试构建**里由 `shangyi-gamespec` / `shangyi-intel` 的 `[dev-dependencies]`
/// 显式启用——R1 前它由上游 vendor crate 带入全图）。
/// 于是"写一个支付矩阵"必须写成：
///
/// ```json
/// {"cells": {"维持,维持": {"focal": {"kind":"literal","value":3}}}}
/// ```
///
/// 而**数值矩阵最自然的写法是直接写数字**。实测一次真实咨询里
/// `gamespec_build` 失败 6 次，其中就包括这一条
/// （`invalid type: number, expected adjacently tagged enum Expr`）。
///
/// 这不是"模型笨"——把一个数写成三层嵌套对象是**我们的**内部表示，
/// 不该泄漏到工具接口上。故在入口处转写。
///
/// ⚠️ 转写时**保留原始数字值对象**（`v.clone()`），不做 `as_f64()` 往返：
/// 转写只该改形状，不该顺手改数值表示。
/// **注意理由的时态**：「`arbitrary_precision` 下前者保留字面量精度」只在
/// **测试构建**成立（该 feature 现由 `shangyi-gamespec` / `shangyi-intel` 的
/// dev-deps 显式启用）；正式构建里 feature 已不存在，`Value` 的数字在解析期
/// 即为 f64/u64/i64，`v.clone()` 与 `as_f64()` 没有精度差别。
/// 故保留 `v.clone()` 的现行理由只有一条：**形状转写不碰数值**。
///
/// ## 2. `payoffs` 缺 `form` 时补 `"normal"`
///
/// 实测报错 `missing field form`。`form` 是判别标签，而 MVP 只有一种形态
/// （`normal`）——要模型每次写一个只有一个合法值的标签，纯属负担。
/// 有 `cells` 就一定是 normal（别的形态没有 `cells`），故补全**不猜**。
///
/// ## 3. 双重编码的 JSON 字符串 → 解析它
///
/// 实测报错 `invalid type: string "{\"spec\": {...}}"`。模型把整个对象
/// 序列化成字符串又塞进字符串里（函数调用参数层已是字符串，套两层很常见）。
/// 拒掉并让它重发是浪费一个 turn；解析一次即可，且失败时报错仍准确。
/// 把 `patch` 深合并进 `base`，返回合并结果。
///
/// # 语义（刻意保持可预测）
///
/// | 情形 | 结果 |
/// |---|---|
/// | 两边都是对象 | 逐键递归合并（`patch` 没提到的键**保留**） |
/// | `patch` 是数组 | **整体替换**（数组合并没有"对"的定义，猜会出错） |
/// | `patch` 是标量 / null | 替换 |
/// | 类型不匹配（如 base 是对象、patch 是字符串） | 替换（以 `patch` 为准） |
///
/// 这套语义让"先搭骨架、再补参数"成立：第二次调用只带 `params`，
/// 而 `players`/`strategies`/`payoffs` 保持第一次写下的内容。
///
/// ⚠️ 它**不校验**——合并结果仍要走 `GameSpec::validate`（铁律 2）。
/// 深合并只是个便利，绝不能成为绕过假设确认的路径。
fn deep_merge(mut base: serde_json::Value, patch: serde_json::Value) -> serde_json::Value {
    match (&mut base, patch) {
        (serde_json::Value::Object(b), serde_json::Value::Object(p)) => {
            // If an adjacently/internally tagged variant (`kind` or `form`) changed,
            // replace cleanly so stale variant fields cannot leak across merges.
            let kind_changed =
                matches!((b.get("kind"), p.get("kind")), (Some(bk), Some(pk)) if bk != pk);
            let form_changed =
                matches!((b.get("form"), p.get("form")), (Some(bf), Some(pf)) if bf != pf);
            if kind_changed || form_changed {
                return serde_json::Value::Object(p);
            }
            for (k, v) in p {
                match b.remove(&k) {
                    Some(old) => {
                        b.insert(k, deep_merge(old, v));
                    }
                    None => {
                        b.insert(k, v);
                    }
                }
            }
            serde_json::Value::Object(b.clone())
        }
        (_, patch) => patch,
    }
}

/// 集成测试回放入口——见 `lib.rs::test_support` 的说明。
pub(crate) fn normalize_spec_for_replay(
    v: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    normalize_spec_input(v)
}

fn normalize_spec_input(args: &serde_json::Value) -> Result<serde_json::Value, String> {
    // ── 3. 双重编码 ──
    let mut v = if let Some(s) = args.as_str() {
        match serde_json::from_str::<serde_json::Value>(s) {
            Ok(parsed) => parsed,
            Err(e) => {
                return Err(format!(
                    "参数是字符串，但内容不是合法 JSON：{e}\n{}",
                    diagnose_unbalanced_json(s)
                ))
            }
        }
    } else {
        args.clone()
    };

    // ── 3b. 解包 ──
    v = unwrap_envelope(&v);
    let Some(obj) = v.as_object_mut() else {
        return Err(format!(
            "GameSpec 必须是 JSON 对象，实际收到**{}**。\n\
             本工具只要一个顶层对象（`{{\"game_id\": ..., \"players\": [...]}}`），\
             也接受包在 `spec` 键里；数组/字符串/数字都不是 GameSpec。",
            json_kind(&v)
        ));
    };

    // ── 3c. 已知容器位置上的「字符串化 JSON」还原 ──
    if let Err(msg) = unwrap_spec_containers(obj) {
        return Err(msg);
    }
    // ── 3d. `meta` 的值必须是字符串：数组/数字转写而非拒绝 ──
    coerce_meta_values(obj);

    if let Some(payoffs) = obj.get_mut("payoffs") {
        if let Some(p) = payoffs.as_object_mut() {
            // 2. 补 form——**两种「有 cells 但缺 form」的写法都要认**：
            //    · `{"cells": {...}}`            ——cells 直接挂在外层
            //    · `{"values": {"cells": {...}}}`——cells 在 values 里（规范形态）
            //
            // ⚠️ 早先只看**外层**的 `cells`，于是第二种形态（也是最规范的形态）
            // 缺 `form` 时不补 → 直接报 `missing field values`（因为 serde 按
            // `form` 的取值去挑 content 键，没 form 就无从挑）。
            // 实测：模型正是这么写的（它把 cells 放对了位置，只是漏了标签），
            // 而报错说 "missing field values" ——**指向了它明明写对的那个字段**。
            // 由本模块的 `missing_form_defaults_to_normal` 测试锁住。
            let has_cells = p.contains_key("cells")
                || p.get("values").map(|v| v.get("cells").is_some()).unwrap_or(false);
            if !p.contains_key("form") && has_cells {
                p.insert("form".into(), serde_json::Value::String("normal".into()));
            }
            // 1. 转写 cells 里的裸数字/裸字符串
            if let Some(cells) = p.get_mut("values").and_then(|x| x.get_mut("cells")) {
                rewrite_expr_leaves(cells);
            }
        }
    }
    // params.*.value 同样是 Expr
    if let Some(params) = obj.get_mut("params").and_then(|x| x.as_object_mut()) {
        for (_, p) in params.iter_mut() {
            if let Some(val) = p.get_mut("value") {
                *val = to_expr(val);
            }
        }
    }

    // 4. dynamics / information 的**自然写法** → 邻接标记形态
    if let Some(d) = obj.get_mut("dynamics") {
        *d = normalize_dynamics(d);
    }
    if let Some(i) = obj.get_mut("information") {
        *i = normalize_information(i);
    }
    Ok(v)
}

/// 剥掉工具调用外层的包装，露出真正的 GameSpec。
///
/// # 为什么需要（实测：一次咨询里 21 次调用栽在这里）
///
/// 模型把整个 spec 塞进一个键里，键名试过三种：
///
/// ```text
/// {"spec":      "{...}"}   ← 10 次「GameSpec 必须是 JSON 对象」
/// {"arguments": "{...}"}   ← 10 次「missing field game_id」（2011 行的参数面泄漏）
/// ```
///
/// 前者是它自己发明的包装（`spec` 是合理的直觉：工具名就叫 gamespec_build）；
/// 后者是**我们泄漏给它的**——`agent.rs` 里那段把工具产物回灌进上下文时用了
/// `{"arguments": ...}` 的形状，模型照抄成了请求形状。
///
/// 两种都不该由模型来猜。故：**顶层有 `game_id` 就认顶层；否则逐个剥**
/// （`spec` → `arguments`），值本身还可以是「装着 JSON 的字符串」，一并解析。
/// 剥不动就原样返回，让后面的报错说清收到了什么。
fn unwrap_envelope(v: &serde_json::Value) -> serde_json::Value {
    let mut cur = v.clone();
    // 上界 4 次：防病态输入下的无限自指（每轮必须真的剥掉一层）
    for _ in 0..4 {
        let Some(o) = cur.as_object() else { break };
        if o.contains_key("game_id") || o.contains_key("players") || o.contains_key("strategies") {
            break;
        }
        let inner = o
            .get("spec")
            .or_else(|| o.get("arguments"))
            .or_else(|| o.get("input"))
            .cloned();
        match inner {
            Some(serde_json::Value::String(s)) => match serde_json::from_str(&s) {
                Ok(parsed) => cur = parsed,
                Err(_) => break,
            },
            Some(other) => cur = other,
            None => break,
        }
    }
    cur
}

/// JSON 值的类型名（中文），用于把「收到了什么」写进报错。
fn json_kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "布尔值",
        serde_json::Value::Number(_) => "数字",
        serde_json::Value::String(_) => "字符串",
        serde_json::Value::Array(_) => "数组",
        serde_json::Value::Object(_) => "对象",
    }
}

/// 已知容器位置上的「字符串化 JSON」还原为真容器。
///
/// # 为什么需要（实测：一次咨询里 21 次调用栽在这里）
///
/// 模型把**子对象**序列化成字符串再塞进去，键各不相同：
/// `payoffs`（8 次）、`params`（8 次）、`strategies`（若干）。
/// 它们全都长这样：
///
/// ```text
/// "payoffs": "{\"form\": \"normal\", \"values\": {...}}"
/// "params":  "{}"
/// ```
///
/// 而每一个都以 `invalid type: string, expected a struct/map` 被拒——
/// 错误是对的，但**没有任何提示指向"你多套了一层引号"**。
/// 模型于是继续换键、换写法，而不是去掉引号。
///
/// 这几个键在 GameSpec 里**只可能是容器**，所以在入口处解析一次是无歧义的：
/// 解析成功即采用；解析失败则**指名道姓**地报，而不是留给 serde 去说
/// "expected a map"（那句话没告诉模型错在哪个键上）。
fn unwrap_spec_containers(obj: &mut serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    // `dynamics` / `information` 的**裸字符串是合法写法**（`"one_shot"`、`"repeated"`
    // 是 `normalize_dynamics` 明确接受的形态）。若在这里把解析失败的字符串
    // 一律报错，就会把那条正常路径堵死——故这两个键只在**能解析出对象**时才还原，
    // 其余情形原样留给下游的 `normalize_*` 处理。
    const LENIENT: [&str; 2] = ["dynamics", "information"];
    const STRICT: [&str; 4] = ["payoffs", "strategies", "params", "meta"];

    for key in LENIENT {
        let Some(val) = obj.get_mut(key) else { continue };
        let serde_json::Value::String(s) = val else {
            continue;
        };
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s) {
            if parsed.is_object() {
                *val = parsed;
            }
        }
    }

    for key in STRICT {
        let Some(val) = obj.get_mut(key) else { continue };
        let serde_json::Value::String(s) = val else {
            continue;
        };
        match serde_json::from_str::<serde_json::Value>(s) {
            Ok(parsed) => *val = parsed,
            Err(e) => {
                return Err(format!(
                    "`{key}` 的值是一个**字符串**，且里面的内容不是合法 JSON：{e}\n\
                     → `{key}` 要的是**容器本身**（对象），不是它的字符串形式。\
                     去掉外层引号与转义即可；**若内容确实太长写不下**，\
                     先建最小骨架，再带 `merge: true` 分几次调用增量补。"
                ))
            }
        }
    }
    Ok(())
}

/// `meta` 的值统一成字符串。
///
/// # 为什么转写而不是拒绝（实测：整个 spec 因为一个展示字段被拒）
///
/// `meta` 是 `BTreeMap<String, String>`——**纯展示**元数据（标题、单位、
/// 计价说明、理论清单），不参与任何计算。模型却很自然地写出：
///
/// ```text
/// "meta": {"framework": ["rubinstein-bargaining", "nash-bargaining", "BATNA"]}
/// ```
///
/// 这被拒的理由是 `invalid type: sequence, expected a string`——
/// **为一个标题字段的容器类型，把一份信息完整的博弈定义整份退回去**，
/// 代价明显不成比例：内容一个不少，只是装法不同。
///
/// 故：数组用「、」连接、数字与布尔转成字面量、对象转成紧凑 JSON。
/// 转写不新增内容、不改数值语义，且 `meta` 从不进入求解。
fn coerce_meta_values(obj: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(serde_json::Value::Object(meta)) = obj.get_mut("meta") else {
        return;
    };
    for (_, v) in meta.iter_mut() {
        let replacement = match &*v {
            serde_json::Value::String(_) => None,
            serde_json::Value::Array(items) => {
                let parts: Vec<String> = items
                    .iter()
                    .map(|x| match x {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect();
                Some(serde_json::Value::String(parts.join("、")))
            }
            other => Some(serde_json::Value::String(other.to_string())),
        };
        if let Some(r) = replacement {
            *v = r;
        }
    }
}

/// 把 `dynamics` 的自然写法归一化成 `{"form": ..., "values": {...}}`。
///
/// # 为什么必须宽容（实测：同一处连续失败 7 次）
///
/// 模型依次试过这些**都很合理**的写法，全部被拒：
///
/// ```text
/// {"repeated": true}                      ← 变体名作键
/// {"form": "repeated"}                    ← 缺 content
/// {"type": "repeated"}                    ← tag 键名猜成 type
/// {"form": "repeated", "type": "repeated"}← 两个都写
/// ```
///
/// 而每次拿到的错误都是 `missing field params`（那曾是 content 的键名，
/// 与顶层参数表撞名）——**错误指向了错误的位置**，于是它一直去补顶层字段。
/// 根因已在 `shangyi-gamespec` 修掉（content 改名 `values`），
/// 但**接口本身仍要求模型知道我们的标记约定**，那是不必要的负担。
///
/// 故这里接受下列全部写法：
///
/// | 输入 | 归一化为 |
/// |---|---|
/// | `"repeated"` / `"one_shot"`（裸字符串） | `{"form": …}` |
/// | `{"type": "repeated", "discount": 0.9}` | `{"form":"repeated","values":{"discount":0.9}}` |
/// | `{"form": "repeated", "discount": 0.9}` | 同上（散落的键收进 values） |
/// | `{"repeated": {"discount": 0.9}}` | 同上 |
/// | `{"repeated": true}` | `{"form":"repeated"}` ⇒ 语义不全，由**报错**指出缺 discount |
/// | 已是 `{"form":…,"values":…}` | 原样 |
///
/// ⚠️ 归一化**不补数值**：`{"repeated": true}` 缺 `discount` 时不得编一个
/// 默认折现因子——那是无来源的数字。它会在校验/求解时被如实拒绝。
fn normalize_dynamics(v: &serde_json::Value) -> serde_json::Value {
    // 裸字符串
    if let Some(s) = v.as_str() {
        return serde_json::json!({ "form": s });
    }
    let Some(o) = v.as_object() else {
        return v.clone();
    };
    // 已是规范形态（有 form + values）：只统一 tag 键名
    let tag = o
        .get("form")
        .or_else(|| o.get("type"))
        .or_else(|| o.get("structure"))
        .and_then(|x| x.as_str());
    // 变体名作键：{"repeated": {...}}
    for (k, val) in o.iter() {
        if (k == "repeated" || k == "one_shot") && !matches!(val, serde_json::Value::Null) {
            let mut out = serde_json::Map::new();
            out.insert("form".into(), serde_json::Value::String(k.clone()));
            if let Some(inner) = val.as_object() {
                let mut vals = serde_json::Map::new();
                for (ik, iv) in inner.iter() {
                    // 内层若自带 form/values，直接用它
                    if ik == "values" {
                        if let Some(vo) = iv.as_object() {
                            for (vk, vv) in vo.iter() {
                                vals.insert(vk.clone(), vv.clone());
                            }
                        }
                    } else if ik != "form" {
                        vals.insert(ik.clone(), iv.clone());
                    }
                }
                if !vals.is_empty() {
                    out.insert("values".into(), serde_json::Value::Object(vals));
                }
            }
            return serde_json::Value::Object(out);
        }
    }
    let Some(tag) = tag else {
        return v.clone();
    };
    let mut out = serde_json::Map::new();
    out.insert("form".into(), serde_json::Value::String(tag.to_string()));
    // content：优先取已有的 values/params；否则把散落的键收进来
    let mut vals = serde_json::Map::new();
    if let Some(existing) = o
        .get("values")
        .or_else(|| o.get("params"))
        .and_then(|x| x.as_object())
    {
        for (k, val) in existing.iter() {
            vals.insert(k.clone(), val.clone());
        }
    }
    for (k, val) in o.iter() {
        if matches!(
            k.as_str(),
            "form" | "type" | "structure" | "values" | "params" | "note"
        ) {
            continue;
        }
        vals.insert(k.clone(), val.clone());
    }
    // ── 统一的字段级归一化（按内容键，与所在变体无关）──
    //
    // ⚠️ `horizon` 是 `Option<u32>`，而"无限期"是重复博弈**最常见的语义**——
    // 模型自然写 `"horizon": "indefinite"`（实测 run29），而那是解析错误。
    //
    // 正确处理不是让它改写成一个大数字（那会**改变数学含义**：
    // 有限期很大 ≠ 无限期，贴现因子的收敛性质不同），而是**映射到 `null`**
    // ——`Option<u32>` 的 `None` 正是"无限期"的表达。
    if let Some(h) = vals.get_mut("horizon") {
        // ⚠️ **任何非法值的 horizon 都映射为 `null`**，而不只是"无限期"那几个词。
        //
        // # 为什么放宽到"非数值一律 null"（run30 实测）
        //
        // `horizon` 是 `Option<u32>`——**它本来就允许不指定**
        // （不指定 = 无限期，那是重复博弈的常见建模）。
        //
        // 而模型会往这里写**各种非数值**：
        // - `"indefinite"` / `"infinite"` / `"无限"`（语义就是无限期）
        // - `"待确认（折现因子缺失）"`（模型在表达"这个值我拿不到"）
        // - `null`
        //
        // 后者的意图与 `null` **完全一致**：`None` 正是"未指定/无限期"。
        // 把它当解析错误是**过度严格**——它逼模型为"我拿不到这个值"
        // 找一个合法写法，而那正是「未取得」该有的表达。
        //
        // # 代价与边界（如实说明）
        //
        // 若有模型把**具体年数**写成 `"三年"`，这里会把它当 None——
        // 丢失了"三年"这个信息。但这可接受：`u32` 与"三年"之间的转换
        // 是**语义判断**，引擎不该猜（猜错就是编数）。宁可当"未指定"，
        // 并在输出里如实提示，也不要猜一个数。
        let is_number = matches!(h, serde_json::Value::Number(_));
        if !is_number {
            *h = serde_json::Value::Null;
        }
    }
    // 只有非空才写 values：`{"form":"one_shot"}` 不该带一个空对象，
    // 那会让 `OneShot` 变体（无字段）反序列化失败。
    if !vals.is_empty() {
        out.insert("values".into(), serde_json::Value::Object(vals));
    }
    serde_json::Value::Object(out)
}

/// `information` 的同款归一化：`"complete"` / `{"type":"complete"}` /
/// `{"complete": true}` / `{"form":"incomplete","types":{…}}` 都接受。
///
/// 与 `dynamics` 同因：这些是**我们的标记约定**，不该成为模型的门槛。
fn normalize_information(v: &serde_json::Value) -> serde_json::Value {
    // ⚠️ `Information` 用的是 **internally tagged**（`tag = "type"`），
    // 且它的两个变体都**不含数值字段**（`Complete` 无字段、`Incomplete`
    // 只有字符串数组）——故它不受 `arbitrary_precision` 那个坑影响
    // （该坑只发生在含 `f64` 的 internally/untagged 枚举上）。
    // 这里必须输出 `type` 而不是 `form`。
    if let Some(s) = v.as_str() {
        return serde_json::json!({ "type": s });
    }
    let Some(o) = v.as_object() else {
        return v.clone();
    };
    // 变体名作键
    for (k, val) in o.iter() {
        if (k == "complete" || k == "incomplete") && !matches!(val, serde_json::Value::Null) {
            let mut out = serde_json::Map::new();
            out.insert("type".into(), serde_json::Value::String(k.clone()));
            // internally tagged ⇒ 内层字段**直接摊平**（没有 content 容器）
            if let Some(inner) = val.as_object() {
                for (ik, iv) in inner.iter() {
                    out.insert(ik.clone(), iv.clone());
                }
            }
            return serde_json::Value::Object(out);
        }
    }
    let tag = o
        .get("form")
        .or_else(|| o.get("type"))
        .and_then(|x| x.as_str());
    let Some(tag) = tag else {
        return v.clone();
    };
    // internally tagged ⇒ 其余字段**摊平**在顶层，不放进任何 content 键
    let mut out = serde_json::Map::new();
    out.insert("type".into(), serde_json::Value::String(tag.to_string()));
    for (k, val) in o.iter() {
        if matches!(k.as_str(), "form" | "type") {
            continue;
        }
        // 有人可能把内容包进 values/params——拆开摊平
        if matches!(k.as_str(), "values" | "params") {
            if let Some(inner) = val.as_object() {
                for (ik, iv) in inner.iter() {
                    out.insert(ik.clone(), iv.clone());
                }
            }
            continue;
        }
        out.insert(k.clone(), val.clone());
    }
    serde_json::Value::Object(out)
}

/// 深度遍历，把叶子上的裸数字/裸字符串转成 `Expr` 的邻接形态。
///
/// `None` 原样保留（"该支付未取得"是合法状态，不能变成 0）。
fn rewrite_expr_leaves(node: &mut serde_json::Value) {
    match node {
        serde_json::Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                // 已经是合法 Expr 形态的对象（含 kind）不再下钻——
                // 否则会把 {"kind":"literal","value":3} 里的 3 当成叶子转写两次。
                if v.get("kind").is_some() {
                    continue;
                }
                if v.is_object() {
                    rewrite_expr_leaves(v);
                } else {
                    *v = to_expr(v);
                }
            }
        }
        other => {
            *other = to_expr(other);
        }
    }
}

/// 单个值 → `Expr` 的邻接形态。已是该形态则原样返回。
fn to_expr(v: &serde_json::Value) -> serde_json::Value {
    if v.get("kind").is_some() {
        return v.clone();
    }
    match v {
        // 保留原始数字对象：arbitrary_precision 下它保有字面量精度
        serde_json::Value::Number(_) => {
            serde_json::json!({"kind": "literal", "value": v.clone()})
        }
        // 裸字符串 ⇒ 参数引用（这是唯一不歧义的解释：支付值只能是数或参数名）
        serde_json::Value::String(s) => {
            serde_json::json!({"kind": "ref", "value": s.clone()})
        }
        // `null` 原样：它表示"未取得"，不该被静默当成 0
        other => other.clone(),
    }
}

/// 把参与者画像渲染成 persona 的角色卡文本。
///
/// # 为什么从画像生成而不是让模型写角色卡
///
/// persona 扮演的是**真实存在的对手**，其行为风格必须可追溯到来源
/// （用户口述、财报、新闻）。让模型"写一个竞争对手的人设"会产出读起来
/// 很像、但没有任何依据的东西——而仿真结论会继承那份无依据。
///
/// 故这里只做**转述**：画像里有什么就写什么，缺的部分如实写「未取得」。
/// 这也是为什么本函数不含任何推断（不猜对手"大概是保守型"）。
fn describe_profile_for_persona(p: &shangyi_intel::ParticipantProfile) -> String {
    let mut s = String::new();
    let style = if p.common.decision_style.trim().is_empty() {
        "未取得"
    } else {
        p.common.decision_style.trim()
    };
    s.push_str(&format!("决策风格：{style}\n"));
    if !p.common.interests.is_empty() {
        s.push_str(&format!("关切利益：{}\n", p.common.interests.join("、")));
    }
    if !p.common.resources.is_empty() {
        s.push_str(&format!("可动用资源：{}\n", p.common.resources.join("、")));
    }
    if !p.common.power_asymmetry.trim().is_empty() {
        s.push_str(&format!("权力不对称：{}\n", p.common.power_asymmetry.trim()));
    }
    if p.timeline.is_empty() {
        s.push_str("可观察行为记录：**未取得**（无带日期的行为事件）\n");
    } else {
        s.push_str(&format!("可观察行为记录（{} 条）：\n", p.timeline.len()));
        for ev in p.timeline.iter().take(12) {
            s.push_str(&format!(
                "  - {} {:?} 幅度 {:.2}（来源：{}，置信度 {:.2}）\n",
                ev.date,
                ev.event_type,
                ev.magnitude,
                ev.source.locator,
                ev.confidence
            ));
        }
        if p.timeline.len() > 12 {
            s.push_str(&format!("  …（另有 {} 条未列出）\n", p.timeline.len() - 12));
        }
    }
    s
}

/// 生成 `swarm_run` 输出的**如实标注**（哪些结论成立、哪些不成立）。
///
/// 这些标注不是客套话：报告会照抄它们，故它们决定了读者
/// 如何理解那批行动计数。
fn build_swarm_caveats(
    trace: &shangyi_swarm::llm_actor::ActorTrace,
    all_stable: bool,
) -> Vec<String> {
    let mut v = Vec::new();
    if trace.rejected > 0 {
        v.push(format!(
            "本次仿真有 {} 次提议被拒（尝试 {} 次、成功 {} 次）——\
             这些轮次的缺失**不是**「该参与者选择不动」。最后失败原因：{}",
            trace.rejected,
            trace.attempts,
            trace.proposed,
            trace.last_failure.as_deref().unwrap_or("(未记录)")
        ));
    }
    if all_stable {
        v.push(
            "全部轨迹以「稳定」收尾。注意：稳定性判定基于**连续若干轮行动相同**，             而『该轮无提议』也会被计入稳定——故稳定结论的强度受上文拒绝次数约束。"
                .to_string(),
        );
    }
    if trace.novels_missing_ranges > 0 {
        v.push(format!(
            "有 {} 条矩阵外提议缺收益/成本区间，它们会被激励相容判据拒绝——\
             这是设计如此（无依据的非常规行动不得进博弈），不是解析失败。",
            trace.novels_missing_ranges
        ));
    }
    v.push(
        "行动计数是**采样证据**，不是概率分布。它进 `persona_sim` 判据，         由 select 阶段的融合决定权重——**不要**把它当作「对手会怎么做的概率」。"
            .to_string(),
    );
    v
}

/// 构造一个 persona actor 的闭包（由 CLI 组装层提供）。
///
/// # 为什么把 `PersonaSet` 整个传进去（而不是让接线层自己造）
///
/// actor 必须与 `rollout` 使用**同一批** persona：若两边各自构造，
/// 它们可能在字段上有细微差异（决策风格、角色卡文本），而后果是
/// 「actor 在为一批 persona 作答，rollout 在记录另一批的行动」——
/// 这种错配在输出里看不出来，却让行为证据失去意义。
///
/// 故 `PersonaSet` 由执行器构造一次，直接传给接线层；接线层只负责
/// 把 LLM 客户端绑上去（那是它才知道的事：provider、模型名、温度）。
/// # 为什么是 trait 而不是闭包
///
/// 早先写成 `Box<dyn Fn(&str, PersonaSet) -> Box<dyn TracedPersonaActor + 'a>>`，
/// 但那种写法**无法表达"返回的 actor 借用工厂持有的客户端"**：
/// `Fn` 的返回类型被绑定到调用时的生命周期，于是编译器要求
/// 「闭包体的引用不得逃逸」——而那正是这里必须做的事（actor 借用 client）。
///
/// 换成一个带生命周期参数的 trait，`'a` 就成为**工厂与 actor 共享的**
/// 生命周期，约束自然成立。还有一个附带好处：实现者可以持有状态
/// （如默认模型名），而不必靠捕获。
pub trait SwarmActorFactory<'a> {
    /// 用给定的 persona 集合构造一个 actor。
    fn build(
        &self,
        model: &str,
        personas: shangyi_swarm::PersonaSet,
    ) -> Box<dyn shangyi_swarm::llm_actor::TracedPersonaActor + 'a>;
}

/// 构造指向显式 OpenAI Chat Completions 端点的离线测试 `SwarmActorFactory`（不读环境变量、不触及真实密钥）。
#[doc(hidden)]
pub fn openai_swarm_actor_factory_for_test(
    base_url: &str,
    model: &str,
) -> Box<dyn SwarmActorFactory<'static>> {
    let client = shangyi_llm::LlmClient::new(
        shangyi_llm::Endpoint::openai_chat(base_url, "SHANGYI_TEST_KEY", false),
        shangyi_llm::UreqTransport,
    )
    .with_key("test-key");
    let leaked: &'static shangyi_llm::LlmClient<shangyi_llm::UreqTransport> =
        Box::leak(Box::new(client));
    Box::new(crate::providers::LlmSwarmActorFactory::new(leaked, model))
}

impl<'a> DomainExecutor<'a> {
    pub fn new(theory: &'a TheoryLibrary, playbook: &'a Playbook) -> Self {
        Self {
            state: DomainState::default(),
            theory,
            playbook,
            search: None,
            poi: None,
            swarm_actor: None,
            // 默认 scenario 是保守选择：它的路径含全部阶段，cp3 落在 select。
            // 调用方（CLI 的 start_consultation）会按实际模式覆盖。
            mode: ConsultationMode::Scenario,
        }
    }

    /// 绑定案例库的持久化路径（并**立刻尝试加载**已有库）。
    ///
    /// 加载失败时**不静默当空库**：损坏的案例库是几个月积累的样本，
    /// 把它当空库会让后续归档覆盖掉仅有的证据（`CaseLibrary::load` 的文档详述）。
    pub fn with_case_library_path(
        mut self,
        path: std::path::PathBuf,
    ) -> Result<Self, String> {
        match shangyi_kb::CaseLibrary::load(&path) {
            Ok(lib) => {
                self.state.case_library = lib;
                self.state.case_library_path = Some(path);
                Ok(self)
            }
            Err(e) => Err(format!("{e}")),
        }
    }

    /// 绑定 persona actor 的构造方式（`swarm_run` 用）。见 [`SwarmActorFactory`]。
    pub fn with_swarm_actor(mut self, f: Box<dyn SwarmActorFactory<'a> + 'a>) -> Self {
        self.swarm_actor = Some(f);
        self
    }

    /// 绑定知识库（R22）。
    ///
    /// 用 `Arc` 而非所有权：`Wiki` 可能不小（几十页 + 图），
    /// 而它**只读**——共享一份即可，不必每个执行器克隆一份。
    pub fn with_wiki(mut self, w: Option<std::sync::Arc<shangyi_kb::wiki::Wiki>>) -> Self {
        self.state.wiki = w;
        self
    }

    /// 绑定咨询模式（`checkpoint` 的映射依赖它）。
    pub fn with_mode(mut self, mode: ConsultationMode) -> Self {
        self.mode = mode;
        self
    }

    /// 在 `intake` 阶段动态更新咨询模式（如 `select_task_scope` 指定 `mode`）。
    pub fn set_mode(&mut self, mode: ConsultationMode) {
        self.mode = mode;
    }

    /// 在 `intake` 阶段动态切换剧本引用。
    pub fn set_playbook(&mut self, playbook: &'a Playbook) {
        self.playbook = playbook;
    }

    /// 恢复或同步当前任务范围到领域执行器状态（供 `--resume` 重放使用）。
    pub fn set_task_scope(&mut self, scope: Option<TaskScope>) {
        self.state.task_scope = scope;
    }

    /// 登记一条真实用户输入到可信账本（供约束底线与收益证据校验）。
    pub fn record_trusted_user_input(&mut self, text: &str, actor: &str) {
        self.ingest_user_input(text, actor);
    }

    /// 恢复已落盘的 `web_fetch` 抓取结果快照（供 `--resume` 离线重放，不发起网络请求）。
    pub fn restore_fetched_page(&mut self, requested_url: &str, output_json: &str) {
        let Ok(page) = serde_json::from_str::<serde_json::Value>(output_json) else {
            return;
        };
        let Some(final_url) = page.get("url").and_then(serde_json::Value::as_str) else {
            return;
        };
        let Some(text) = page.get("text").and_then(serde_json::Value::as_str) else {
            return;
        };
        if text.trim().is_empty() {
            return;
        }
        self.state
            .fetched_sources
            .borrow_mut()
            .insert(final_url.to_string(), text.to_string());
        self.state.task_sources.insert(final_url.to_string());
        let mut pages = self.state.fetched_pages.borrow_mut();
        if !requested_url.trim().is_empty() {
            pages.insert(requested_url.to_string(), page.clone());
        }
        pages.insert(final_url.to_string(), page);
    }

    /// 恢复已落盘的 `poi_survey` 结构化回执快照（供 `--resume` 离线重放，不发起高德网络请求）。
    pub fn restore_poi_receipts(&mut self, call_id: &str, output_json: &str) {
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(output_json) else {
            return;
        };
        let address = payload
            .get("address")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();
        let radius = payload
            .get("radius")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(500);
        let source = payload
            .get("source")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("高德 POI")
            .to_string();
        let scope = if address.is_empty() {
            format!("poi:{}m", radius)
        } else {
            format!("{address} · {radius} 米")
        };
        if let Some(receipts) = payload.get("receipts").and_then(serde_json::Value::as_array) {
            for (idx, item) in receipts.iter().enumerate() {
                let id = item
                    .get("receipt")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{call_id}#{}", idx + 1));
                let Some(val) = item.get("value").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                self.state.structured_receipts.insert(
                    id.clone(),
                    crate::research_task::StructuredEvidence {
                        receipt: id,
                        kind: "高德 POI 周边计数".into(),
                        scope: scope.clone(),
                        value: val.to_string(),
                        source: source.clone(),
                    },
                );
            }
        }
        self.state.task_retrieval_attempts += 1;
        if let Some(task) = self.state.research_task.as_mut() {
            task.observe_poi(&scope, true, None, self.state.retrieval_attempts.get(), 16);
        }
    }

    /// 恢复已落盘的 `poi_survey` 受阻记录（供 `--resume` 离线重放，维持研究任务尝试计数）。
    pub fn restore_poi_blocked(&mut self, args: &serde_json::Value, detail: &str) {
        let address = args
            .get("address")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();
        let radius = args.get("radius").and_then(serde_json::Value::as_u64).unwrap_or(500);
        let poi_label = format!(
            "poi:{}@{}m",
            if address.is_empty() { "unset" } else { address },
            radius
        );
        self.state.task_retrieval_attempts += 1;
        self.state.task_gaps.push(detail.to_string());
        if let Some(task) = self.state.research_task.as_mut() {
            task.observe_poi(
                &poi_label,
                false,
                Some(detail),
                self.state.retrieval_attempts.get(),
                16,
            );
        }
    }

    /// 恢复已落盘的 `data_ingest` 解析报告快照（供 `--resume` 离线重放，不启动侧车或读取磁盘 CSV）。
    pub fn restore_ingested_report(&mut self, output_json: &str) {
        let Ok(report) = serde_json::from_str::<shangyi_intel::sidecar::IngestReport>(output_json) else {
            return;
        };
        self.state.ingested = self
            .state
            .ingested
            .iter()
            .cloned()
            .chain(std::iter::once(report))
            .collect();
    }

    /// 恢复已落盘的 `swarm_run` 输出与已接受的非常规策略注册（供 `--resume` 离线重放，不发起 LLM 请求）。
    pub fn restore_swarm_output(&mut self, output_json: &str) {
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(output_json) else {
            return;
        };
        self.state.swarm_output = Some(payload.clone());
        let registered = payload
            .get("counts")
            .and_then(|c| c.get("novel_moves_registered"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        if registered > 0 {
            if let Some(spec) = self.state.game_spec.as_mut() {
                if let Some(verdicts) = payload
                    .get("novel_move_verdicts")
                    .and_then(serde_json::Value::as_array)
                {
                    for item in verdicts {
                        if item.get("accepted").and_then(serde_json::Value::as_bool) != Some(true)
                        {
                            continue;
                        }
                        let Some(actor) = item.get("actor").and_then(serde_json::Value::as_str)
                        else {
                            continue;
                        };
                        let Some(action) = item.get("action").and_then(serde_json::Value::as_str)
                        else {
                            continue;
                        };
                        if spec.players.iter().any(|pl| {
                            pl.id == actor
                                && pl.relation_to_game == shangyi_gamespec::RelationToGame::Player
                        }) {
                            let label = format!("[非常规] {action}");
                            let strategies = spec.strategies.entry(actor.to_string()).or_default();
                            if !strategies.contains(&label) {
                                strategies.push(label);
                            }
                        }
                    }
                }
            }
        }
    }

    /// 绑定检索后端。未绑定 ⇒ `web_search`/`web_fetch` 返回 `Blocked`。
    /// 绑定周边数据源。未绑定 ⇒ `poi_survey` 返回 `Blocked`（未取得）。
    pub fn with_poi(mut self, pp: &'a dyn PoiProvider) -> Self {
        self.poi = Some(pp);
        self
    }

    /// 可选绑定（供调用方按"有没有配置 key"决定接线，链式调用不必分叉）。
    pub fn with_poi_opt(mut self, pp: Option<&'a dyn PoiProvider>) -> Self {
        self.poi = pp;
        self
    }

    pub fn with_search(mut self, sp: &'a dyn SearchProvider) -> Self {
        self.search = Some(sp);
        self
    }

    pub fn state(&self) -> &DomainState {
        &self.state
    }

    pub fn trusted_user_inputs(&self) -> &[UserInputFact] { self.state.constraints.user_inputs() }
    pub fn constraint_review_state(&self) -> serde_json::Value { self.state.constraints.snapshot() }

    fn invalidate_constraint_dependents(&mut self) {
        if self.state.attempted_model.is_none() {
            self.state.attempted_model=self.state.game_spec_evidence.clone()
                .or_else(||self.state.game_spec.as_ref().map(|spec|serde_json::to_value(spec).expect("game spec serialization")));
        }
        self.state.game_spec=None;
        self.state.game_spec_evidence=None;
        self.state.equilibria_json=None;
        self.state.distribution=None;
        self.state.selector_scores.clear();
        self.state.consequence_report=None;
        self.state.discovery_report=None;
        self.state.recommendation=None;
        self.state.report_json=None;
        self.state.report_model=None;
        for kind in [ArtifactKind::GameSpecValidated,ArtifactKind::EquilibriumSet,ArtifactKind::SelectionOutput,ArtifactKind::ConsequenceReport,ArtifactKind::DiscoveryReport,
            ArtifactKind::RecommendationPack,ArtifactKind::ReportBundle] {
            if !self.state.constraint_invalidations.contains(&kind) {self.state.constraint_invalidations.push(kind);}
        }
    }

    pub fn ingest_user_input(&mut self, text: &str, actor: &str) {
        let before=self.state.constraints.user_inputs().len();
        let changed=self.state.constraints.record_user_input(text,actor);
        if self.state.constraints.user_inputs().len()>before {
            if !self.state.task_request_text.is_empty() {self.state.task_request_text.push('\n');}
            self.state.task_request_text.push_str(text);
        }
        if changed || (actor=="user" && mentions_unknown_payoff(text)) {
            self.invalidate_constraint_dependents();
            if mentions_unknown_payoff(text) {
                let raw=self.state.attempted_model.clone().unwrap_or_else(||serde_json::json!({}));
                self.state.decision_gaps=self.payoff_evidence_gaps(&raw);
            }
        }
    }

    /// 注入一个已构建的 GameSpec（供"由测试/人工确认后写入"的路径）。
    ///
    /// 这是 `gamespec_build` 之外的入口，因为**建模本身**在本架构里由
    /// 确定性代码 + 硬假设确认完成，而非由模型自由生成数值。模型可以提议
    /// 结构（有哪些玩家、哪些策略），但每个数字都必须是
    /// [`shangyi_gamespec::Param`]（带 `source`），否则 `gamespec_build` 拒绝。
    pub fn set_game_spec(&mut self, spec: GameSpec) {
        self.state.game_spec_evidence=None;
        self.state.game_spec = Some(spec);
    }

    pub fn set_profiles(&mut self, profiles: Vec<ParticipantProfile>) {
        self.state.profiles = profiles;
    }

    // ── 各工具的实现 ──

    fn payoff_evidence_gaps(&self, raw:&serde_json::Value)->Vec<EvidenceGap> {
        if self.trusted_user_inputs().is_empty() {return vec![];}
        let shape_available=raw.get("players").is_some_and(|v|v.is_array())
            && raw.pointer("/payoffs/values/cells").is_some_and(|v|v.is_object());
        if !shape_available && !self.trusted_user_inputs().iter().any(|fact|mentions_unknown_payoff(&fact.text)) {
            return vec![]; // Ordinary malformed input remains a format error.
        }
        fn resolve_number(expr:&serde_json::Value,params:&serde_json::Value,depth:usize)->Option<f64> {
            if depth>16{return None;}
            match expr["kind"].as_str() {
                Some("literal")=>expr["value"].as_f64().filter(|v|v.is_finite()),
                Some("ref")=>resolve_number(&params.get(expr["value"].as_str()?)?["value"],params,depth+1),
                _=>None,
            }
        }
        let mut participants:BTreeMap<String,(String,bool)>=BTreeMap::new();
        for model in self.state.attempted_model.iter().chain(std::iter::once(raw)) {
            if let Some(players)=model.get("players").and_then(|v|v.as_array()) {
                for player in players {
                    if let Some(id)=player.get("id").and_then(|v|v.as_str()) {
                        let focal=player["vn_role"]=="focal";
                        participants.entry(id.to_string()).or_insert((player["name"].as_str().filter(|name|!name.trim().is_empty()).unwrap_or(if focal{"你这边"}else{"对方"}).to_string(),focal));
                    }
                }
            }
        }
        let cells=raw.pointer("/payoffs/values/cells").and_then(|v|v.as_object());
        let params=&raw["params"];
        let mut gaps=Vec::new();
        let opponent_count=participants.values().filter(|(_,focal)|!*focal).count();
        for (id,(name,focal)) in &participants {
            let identifies=|quote:&str| quote.contains(id.as_str()) || quote.contains(name.as_str())
                || (*focal && ["我方","我的","本店","本企业"].iter().any(|word|quote.contains(word)))
                || (!*focal && opponent_count==1 && ["对方","对手"].iter().any(|word|quote.contains(word)))
                || (!*focal && ["房东","供应商","竞争者","对手"].iter().any(|word|name.contains(word)&&quote.contains(word)
                    && participants.values().filter(|(other,_)|other.contains(word)).count()==1));
            let unknown_since=self.trusted_user_inputs().iter().filter(|fact|fact.text.split(['。','；','\n','，',',']).any(|clause|mentions_unknown_payoff(clause)
                && (identifies(clause)||clause.contains("双方")||clause.contains("各方")
                    ||(!*focal && ["对方","对手","房东"].iter().any(|word|clause.contains(word)&& (opponent_count==1||name.contains(word)))))))
                .map(|fact|fact.sequence).max();
            let actual_player=raw["players"].as_array().is_some_and(|players|players.iter().any(|p|p["id"]==id.as_str()&&matches!(p["relation_to_game"].as_str(),Some("player"|"我方"|"主要威胁"))));
            let expressions:Vec<_>=cells.into_iter().flat_map(|cells|cells.values()).filter_map(|cell|cell.get(id)).collect();
            let expected=cells.map_or(0,|cells|cells.len());
            let proven=actual_player && expected>0 && expressions.len()==expected && expressions.iter().all(|expr| {
                let Some(key)=expr.get("value").and_then(|v|v.as_str()).filter(|_|expr["kind"]=="ref") else {return false;};
                let Some(param)=params.get(key) else {return false;};
                let Some(value)=resolve_number(&param["value"],params,0) else {return false;};
                let source=&param["source"];
                let quote=source.get("user_quote").or_else(||source.get("quote")).and_then(|v|v.as_str()).unwrap_or("");
                if quote.trim().is_empty() || !identifies(quote) || mentions_unknown_payoff(quote)
                    || ["对吗","是否","能否","会不会","据说","听说","上限","最多","最高","不超过","至少"].iter().any(|word|quote.contains(word))
                    || quote.contains(['?', '？', '吗'])
                    || !["利润","收益","效用","支付"].iter().any(|word|quote.contains(word))
                    || crate::constraints::quoted_single_number(quote)!=Some(value) {return false;}
                let unit=param.get("unit").and_then(|v|v.as_str()).unwrap_or("");
                if (unit.contains("CNY")||unit.contains('元')) && !quote.contains('元') {return false;}
                if (unit.contains("USD")||unit.contains("美元")) && !quote.contains("美元") {return false;}
                match source["kind"].as_str() {
                    Some("user_confirmed"|"confirmed_assumption") => {
                        if source["kind"]=="user_confirmed" && ["假设","设为","用于测算"].iter().any(|word|quote.contains(word)) {return false;}
                        self.trusted_user_inputs().iter().any(|fact|crate::constraints::quote_is_complete_clause(&fact.text,quote)
                            && unknown_since.is_none_or(|unknown|fact.sequence>unknown || (fact.sequence==unknown&&source["kind"]=="confirmed_assumption")))
                    }
                    Some("public_data") if unknown_since.is_none() => source.get("provenance").and_then(|v|v.as_str())
                        .and_then(|url|self.state.fetched_sources.borrow().get(url).cloned()).is_some_and(|body|body.contains(quote)),
                    _=>false,
                }
            });
            if !proven {
                let reason=if unknown_since.is_some() {if *focal {EvidenceGapReason::UnknownOwnPayoff}else{EvidenceGapReason::UnknownOpponentPayoff}}
                    else {EvidenceGapReason::UnconfirmedPayoffAssumption};
                gaps.push(EvidenceGap {reason,participant_id:Some(id.clone()),profile:None,
                    detail:if unknown_since.is_some(){format!("你已说明{name}的关键收益还不清楚。不能把它当作零收入，也不能只用租金收入或模型自行写成已确认的数字替代；相关各方与选择仍须保留。")}
                    else{format!("{name}在各选择下的收益还没有能核对的依据；模型填入一个数字或自行写成已确认，都不能替代你的实际说明或已取得的原文。")}});
            }
        }
        if participants.is_empty() {
            if self.trusted_user_inputs().iter().any(|fact|mentions_unknown_payoff(&fact.text)) {
                gaps.push(EvidenceGap {reason:EvidenceGapReason::MissingDecisionEvidence,participant_id:None,profile:None,
                    detail:"真实用户仍有关键收益未知；当前没有可核对的原参与者及完整收益依据，不能用零占位建立完整博弈。".into()});
            }
        }
        gaps
    }

    fn enforce_payoff_evidence(&mut self,raw:&serde_json::Value)->Option<ExecOutcome> {
        let gaps=self.payoff_evidence_gaps(raw);
        if gaps.is_empty(){return None;}
        if self.state.attempted_model.is_none() || self.state.decision_gaps.is_empty() {
            self.state.attempted_model=Some(raw.clone());
        }
        self.state.decision_gaps=gaps.clone();
        Some(ExecOutcome::InsufficientEvidence{gaps})
    }

    fn check_strategy_constraints(&self,raw:&serde_json::Value)->Result<(),String> {
        if !self.state.constraints.has_limits(){return Ok(());}
        if self.state.constraints.needs_review() || raw.get("constraint_revision").and_then(|v|v.as_u64())!=Some(self.state.constraints.revision()) {
            return Err("用户底线已更新；先constraint_check核对方案，再在GameSpec携带当前constraint_revision重建，旧GameSpec/均衡/确认不能沿用。".into());
        }
        let focal:Vec<_>=raw["players"].as_array().into_iter().flatten().filter(|p|p["vn_role"]=="focal").collect();
        let [player]=focal.as_slice() else {return Err("存在用户硬上限时，必须有唯一明确代表用户的focal主体，不能把限制换绑给另一个参与者。".into());};
        let id=player["id"].as_str().ok_or("focal缺id")?;
        let strategies=raw["strategies"][id].as_array().ok_or("缺少我方策略")?;
        for strategy in strategies {
            let strategy=strategy.as_str().ok_or("策略名须为文本")?;
            let scenario_id=raw["strategy_bindings"][id][strategy].as_str().ok_or_else(||format!("我方策略「{strategy}」必须绑定已计算scenario_id，不能只靠策略名称声明符合上限"))?;
            let scenario=self.state.cost_scenarios.get(scenario_id).ok_or("策略绑定的方案不存在")?;
            let attributes:Vec<ConstraintAttribute>=serde_json::from_value(scenario["constraint_attributes"].clone()).map_err(|_|"方案约束属性缺失，须重新计算")?;
            let assessment=self.state.constraints.assess(&attributes);
            if assessment.status!=Acceptance::Eligible {
                return Err(format!("我方策略「{strategy}」不能作为可执行策略入模：{}。超限报价保留为费用对照，不进入可行策略；未知属性需补齐或交付条件分析。",serde_json::to_string(&assessment).unwrap()));
            }
        }
        Ok(())
    }

    /// `gamespec_build`：从参数表构建 GameSpec。
    ///
    /// **校验在构建期就做**：`GameSpec::validate` 是铁律 2 的可执行落点
    /// （硬假设必须带 `source`）。让它在这里失败，而不是等到求解时——
    /// 越晚发现，用户越可能已经基于坏模型做了判断。
    fn gamespec_build(&mut self, args: &serde_json::Value) -> ExecOutcome {
        // ── 增量构建（`merge: true`）──
        //
        // # 为什么这个开关必须存在（而不是只在报错里建议"分两步"）
        //
        // 实测：`diagnose_unbalanced_json` 早就建议模型「先建最小可解的博弈，
        // 再用**其他工具**补参数」——而**那个"其他工具"当时并不存在**：
        // model_build 阶段的工具只有 `gamespec_build` / `gamespec_validate`
        // / `data_ingest`。于是那条建议是一句**做不到的话**，
        // 模型照着试只会再失败一次。
        //
        // 这是"能力 seam"的一条变体：**报错里许诺的能力必须真的存在**。
        // 许诺而不实现，比不许诺更糟——它让模型把失败归因为"我没读懂说明"。
        //
        // 故此处实现它：`merge: true` 把本次输入**深合并**进会话内已有的
        // GameSpec（对象逐层合并、数组与标量取新值）。
        // 于是"先搭骨架、再补参数"成为一条真的能走通的两步路。
        let merge = args.get("merge").and_then(|v| v.as_bool()).unwrap_or(false);

        // 接受两种传入形态：
        //   1. 直接是 GameSpec（`{"game_id": ..., "players": [...]}`）
        //   2. 包在 `spec` 键里（`{"spec": {...}}`）
        //
        // # 为什么必须宽容（实测暴露）
        //
        // 模型**自发**包了一层（`{"spec": {...}}`），而工具只认形态 1，
        // 于是报 `missing field params`——而真因是**它在 spec 里面**。
        //
        // 更糟的是**这个错误把模型带偏了**：它在总结里写
        // 「`params` 参数被环境层吞掉，模型无法落盘」——把一次参数包装差异
        // 理解成了环境故障，并据此放弃建模。
        //
        // 这里的教训是通用的：**报错若不能指出真因，模型会自己编一个解释**，
        // 而那个解释通常会写进它对用户的说明里。
        let raw = match normalize_spec_input(args) {
            Ok(v) => v,
            Err(msg) => return ExecOutcome::Failed(msg),
        };

        // `merge: true` ⇒ 把本次输入深合并进已有的 spec（若没有则等同新建）。
        // 在**归一化之后、反序列化之前**做：两边都是 JSON 树，
        // 层级合并与「哪些键是容器」的判断在那一层最直观。
        let raw = if merge {
            match &self.state.game_spec {
                Some(existing) => {
                    let base = self.state.game_spec_evidence.clone().unwrap_or_else(||serde_json::to_value(existing).expect("stored game serializes"));
                    deep_merge(base, raw)
                }
                None => raw,
            }
        } else {
            raw
        };

        let spec: GameSpec = match serde_json::from_value(raw.clone()) {
            Ok(s) => s,
            Err(e) => {
                if self.trusted_user_inputs().iter().any(|fact|mentions_unknown_payoff(&fact.text)) {
                    if let Some(outcome)=self.enforce_payoff_evidence(&raw){return outcome;}
                }
                // 报错必须**可操作**：只说 "missing field `name`" 会让模型
                // 反复试错（实测：它连试 3 次同样的错），因为不知道是**哪个**
                // 对象缺 name、也不知道该填什么。
                //
                // 故这里附上完整字段清单——它是稳定的、与 `GameSpec` 的定义一致，
                // 且模型据此能一次改对。
                // 从错误信息里提取缺失的字段名，给出**针对性**的提示。
                //
                // # 为什么值得做这一步
                //
                // 实测：模型在 34 次调用里重复同一个错（`missing field checkpoint`），
                // 每次只看到一行错误。它当然知道缺 `checkpoint`，但不知道
                // **该填什么值**（是 cp2？还是任意字符串？）。
                //
                // 通用字段清单帮不上这种错——问题不在顶层字段，而在**嵌套变体**
                // （`ParamSource::UserConfirmed { checkpoint }`）。故这里按字段名
                // 给**该字段的具体形状**。
                let err = e.to_string();
                let hint = if err.contains("missing field") && err.contains("params") {
                    // 现在 `params` **只有顶层一种含义**了：三处邻接标记枚举
                    // （PayoffForm / Dynamics / Dist）的内容键都已统一为 `values`。
                    // 这条提示因此可以直说。
                    "\n→ 缺的是**顶层参数表** `params`：可以写空对象 `{}`，但不能省略。\n\
                     （若你改的是 `payoffs`/`dynamics` 的内容，那个键叫 `values`——\
                     报 `missing field values` 时才该去动它。）\n"
                } else if err.contains("missing field `values`") {
                    // content 键已统一为 `values`（PayoffForm / Dynamics / Dist 三处）。
                    // 报这个错说明某个邻接标记枚举缺了内容对象。
                    "\n→ **哪个对象缺 content？** 邻接标记枚举（payoffs / dynamics）\
                     都需要一个 `values` 对象装内容：\n\
                     · `payoffs`：{\"form\":\"normal\",\"values\":{\"cells\":{\"策略A,策略B\":{\"p1\":{\"kind\":\"literal\",\"value\":0},\"p2\":{\"kind\":\"literal\",\"value\":0}}}}}\n\
                     · `dynamics`：{\"form\":\"repeated\",\"values\":{\"discount\":0.9}}\n\
                     注意 `OneShot` 是**无内容变体**，写 {\"form\":\"one_shot\"} 即可，不要带 values。\n"
                } else if err.contains("checkpoint") {
                    "\n→ `params.*.source` 若为 user_confirmed，形状必须是：\n\
                     { \"kind\": \"user_confirmed\", \"checkpoint\": \"cp2\" }\n\
                     即 `checkpoint` 字段不能省（它记录该假设是经哪个确认点留档的）。\n"
                } else if e.to_string().contains("provenance") {
                    "\n→ `source.kind = public_data` 时必须给 `provenance`（来源 URL 或工具调用 ID）\n"
                } else if e.to_string().contains("prior_ref") {
                    "\n→ `source.kind = industry_prior` 时必须给 `prior_ref`（先验出处）\n"
                } else {
                    ""
                };
                return ExecOutcome::Failed(format!(
                    "GameSpec 反序列化失败：{e}{hint}\n\
                     \n字段清单（缺一不可）：\n\
                     - `game_id`: string\n\
                     - `players`: 数组，每项含 `id`/`name`/`vn_role`/`relation_to_game`\n\
                     - `strategies`: 对象，键为**玩家 id**（不是 name），值为策略名数组\n\
                     - `payoffs`: {{ \"form\": \"normal\", \"values\": {{ \"cells\": {{ \"策略A,策略B\": {{ \"玩家id\": <参数表达式> }} }} }} }}——\
                     注意 `cells` 必须在 `values` 里（邻接标记），且支付值必须是 \
                     {{\"kind\":\"literal\",\"value\":N}} 或 {{\"kind\":\"ref\",\"value\":\"参数名\"}}，裸数字会被拒\n\
                     - `params`: 对象（可为空 {{}}）\n\
                     \n\
                     若这份 spec 太长、一次写不完整，**第一步可先搭「最小可校验骨架」**\
                     （含 `players`、`strategies`、至少各策略组合的初始字面量 `payoffs.values.cells`，以及 `params: {{}}`），\
                     **再带 `merge: true` 分步补齐 `params` 与 `ref` 引用**——\
                     注意 `cells` 本身不能为空 `{{}}`，且仅可使用已获来源证实或经用户确认的数值。\n\
                     \n\
                     \n常见错误：\n\
                     ① 忘了 `players[].name`（本项必填，即使与 id 相同也要写）\n\
                     ② `strategies` 用了中文玩家名而非 **id** 作键\n\
                     ③ `payoffs.cells` 的外层键不是逗号连接的策略名组合"
                ));
            }
        };
        if let Some(outcome)=self.enforce_payoff_evidence(&raw){return outcome;}
        if let Err(reason)=self.check_strategy_constraints(&raw){return ExecOutcome::Failed(reason);}
        if let Err(errs) = spec.validate() {
            return ExecOutcome::Failed(render_validation(&errs));
        }
        let summary = summarize_spec(&spec);
        self.state.game_spec_evidence=Some(raw);
        self.state.decision_gaps.clear();
        self.state.attempted_model=None;
        self.state.game_spec = Some(spec);
        ExecOutcome::Ok(summary)
    }

    fn gamespec_validate(&mut self, args: &serde_json::Value) -> ExecOutcome {
        // Only an explicitly empty request selects the stored model. A malformed
        // supplied model must never receive a success result for a different one.
        let Some(input) = args.as_object() else {
            return ExecOutcome::Failed("gamespec_validate 参数必须是对象；校验已建模型请传 {}，校验另一个完整模型请传 {\"spec\":{...}}。".into());
        };
        let use_current = input.is_empty();
        let raw_for_evidence=if use_current {
            self.state.game_spec_evidence.clone().or_else(||self.state.game_spec.as_ref().map(|s|serde_json::to_value(s).expect("stored game serializes")))
        } else if input.contains_key("spec") {
            (input.len()==1).then(||input["spec"].clone())
        } else {Some(args.clone())};
        let spec_owned;
        let spec: &GameSpec = if use_current {
            match &self.state.game_spec {
                Some(s) => {spec_owned=s.clone();&spec_owned},
                None => {
                    return ExecOutcome::Failed(
                        "本次 {} 请求校验当前模型，但会话内尚无 GameSpec。请先调用 gamespec_build 成功保存模型。"
                            .into(),
                    );
                }
            }
        } else {
            // Preserve the existing direct-GameSpec API as well as the documented
            // wrapper. Neither path mutates or substitutes the stored model.
            let (provided, prefix) = match input.get("spec") {
                Some(provided) if input.len() == 1 => (provided, "$.spec"),
                Some(_) => return ExecOutcome::Failed("使用 spec 包装时不能再同时传其他顶层字段；请只传 {\"spec\":完整GameSpec}。如只校验已建模型，直接传 {}。".into()),
                None => (args, "$"),
            };
            match serde_path_to_error::deserialize::<_, GameSpec>(provided.clone()) {
                Ok(s) => {
                    spec_owned = s;
                    &spec_owned
                }
                Err(e) => {
                    if self.trusted_user_inputs().iter().any(|fact|mentions_unknown_payoff(&fact.text)) {
                        if let Some(raw)=&raw_for_evidence {
                            if let Some(outcome)=self.enforce_payoff_evidence(raw){return outcome;}
                        }
                    }
                    let path = e.path().to_string();
                    let location = if path.is_empty() || path == "." {
                        prefix.to_string()
                    } else {
                        format!("{prefix}.{path}")
                    };
                    let stored = match &self.state.game_spec {
                        Some(current) => format!("会话已有模型 game_id={}。若只需检查刚刚构建的模型，下一次 gamespec_validate 传 {{}} 即可，无需重写矩阵。", current.game_id),
                        None => "会话尚无已建模型；请先用 gamespec_build 构建成功。".to_string(),
                    };
                    return ExecOutcome::Failed(format!(
                        "本次传入的 GameSpec 反序列化失败，字段位置 {location}：{}。\n\
                         本次输入未通过；没有改为校验或覆盖会话模型。\n\
                         GameSpec 顶层必须包含 game_id、players、strategies、payoffs、params。players 每项是含 id/name/vn_role/relation_to_game 的对象，不是玩家名字字符串。\n\
                         payoffs 的最小结构为 {{\"form\":\"normal\",\"values\":{{\"cells\":{{\"策略A,策略B\":{{\"玩家A\":{{\"kind\":\"ref\",\"value\":\"参数名\"}}}}}}}}}}。这是局部结构示例，实际须填齐全部策略组合及玩家，引用参数必须存在；不能写成 {{\"玩家名\":[[收益矩阵]]}} 或把 form 放到顶层。\n\
                         若错误位于 dynamics，其结构用 {{\"form\":\"one_shot\"}} 或 {{\"form\":\"repeated\",\"values\":{{\"discount\":数值}}}}；不要把 payoffs 与 dynamics 的 form 混淆。\n\
                         {stored}\n\
                         如需修改模型，先用 gamespec_build 保存修正，再用 {{}} 校验；validate 本身不保存输入。",
                        e.inner()
                    ));
                }
            }
        };
        if let Some(raw)=&raw_for_evidence {
            if let Some(outcome)=self.enforce_payoff_evidence(raw){return outcome;}
            if let Err(reason)=self.check_strategy_constraints(raw){return ExecOutcome::Failed(reason);}
        }
        match spec.validate() {
            Ok(()) => ExecOutcome::Ok(serde_json::json!({
                "valid": true,
                "validated_target": if use_current {"current"} else {"provided"},
                "game_id": spec.game_id,
                "validation_scope": "gamespec_structure_and_parameter_sources"
            }).to_string()),
            Err(errs) => ExecOutcome::Failed(render_validation(&errs)),
        }
    }

    /// `solve_equilibria`：确定性求解。**铁律 1 的核心落点。**
    ///
    /// 实现委托给 [`solve_spec_to_json`]：解集口径只允许有一份——
    /// `shangyi solve`（离线子命令）与工具路径必须走同一段代码。
    fn solve_equilibria(&mut self, args: &serde_json::Value) -> ExecOutcome {
        if !self.state.decision_gaps.is_empty() {
            return ExecOutcome::InsufficientEvidence{gaps:self.state.decision_gaps.clone()};
        }
        if let Some(raw)=self.state.game_spec_evidence.clone().or_else(||self.state.game_spec.as_ref().map(|s|serde_json::to_value(s).expect("stored game serializes"))) {
            if let Some(outcome)=self.enforce_payoff_evidence(&raw){return outcome;}
            if let Err(reason)=self.check_strategy_constraints(&raw){return ExecOutcome::Failed(reason);}
        }
        let Some(spec) = &self.state.game_spec else {
            return ExecOutcome::Failed(
                "会话内尚无 GameSpec。请先调用 `gamespec_build` 并通过 checkpoint② 确认。"
                    .into(),
            );
        };
        match solve_spec_to_json(spec, args) {
            Ok(json) => {
                self.state.equilibria_json = Some(json.clone());
                ExecOutcome::Ok(json)
            }
            Err(e) => ExecOutcome::Failed(e),
        }
    }

    /// `select_equilibria`：单选择器评估。
    fn select_equilibria(&mut self, args: &serde_json::Value) -> ExecOutcome {
        let ids = match self.equilibrium_ids() {
            Ok(v) => v,
            Err(e) => return e,
        };
        let selector = args
            .get("selector")
            .and_then(|v| v.as_str())
            .unwrap_or("risk_dom");
        let sid = match parse_selector(selector) {
            Some(s) => s,
            None => {
                return ExecOutcome::Failed(format!(
                    "未知选择器 `{selector}`；可用：{}",
                    SelectorId::ALL.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" / ")
                ))
            }
        };
        // ── 解析分数（两种形态：裸数字数组，或带理由的对象数组）──
        //
        // ⚠️ 两点此前是**能力缺失**（实测审计）：
        //
        // 1. `normalize_selector` **从未被调用**。它的文档说得很清楚：
        //    "不同判据的原始输出量纲不同（风险占优给 0/1 方向判断，
        //    `persona_sim` 给 rollout 落点频率）。若不归一化，量纲大的判据
        //    会支配融合结果——那是实现缺陷而非算法意图。"
        //    ⇒ 于是调用方给 [90, 10]（计数）与 [0.9, 0.1]（比例）
        //    会得到**不同的融合结果**，而两者本应等价。
        //
        // 2. `rationale` 被硬编码成"由判据计算侧给出"、`evidence` **完全为空**。
        //    而 `rationale` 的文档写着"**进报告**"、`evidence` 写着
        //    "机器可审计证据（收敛轨迹摘要、逐对比较数值）"。
        //    ⇒ 报告里的"为什么选这个均衡"因此**永远是同一句话**。
        let raw = match args.get("scores").and_then(|v| v.as_array()) {
            Some(a) => a.clone(),
            None => {
                return ExecOutcome::Failed(format!(
                    "`select_equilibria` 需要 `scores` 数组（每个均衡一个分数）。\
                     判据 `{selector}` 的具体计算由 shangyi-selection 的对应函数产出——\
                     本执行器不替模型编造分数（铁律 1）。"
                ))
            }
        };
        let mut scores: Vec<SelectorScore> = raw
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let eq_id = ids.get(i).cloned().unwrap_or_else(|| format!("e{}", i + 1));
                match v {
                    // 形态 2：带理由与证据（**推荐**——报告需要它）
                    serde_json::Value::Object(o) => {
                        let w = o.get("score").and_then(|x| x.as_f64()).unwrap_or(0.0);
                        let rationale = o
                            .get("rationale")
                            .and_then(|x| x.as_str())
                            .unwrap_or("（未给理由）");
                        let mut sc = SelectorScore::new(sid, eq_id, w, rationale);
                        if let Some(ev) = o.get("evidence").and_then(|x| x.as_object()) {
                            for (k, val) in ev {
                                sc = sc.with_evidence(
                                    k.clone(),
                                    val.as_str()
                                        .map(str::to_string)
                                        .unwrap_or_else(|| val.to_string()),
                                );
                            }
                        }
                        sc
                    }
                    // 形态 1：裸数字（兼容；但报告里不会有理由）
                    other => SelectorScore::new(
                        sid,
                        eq_id,
                        other.as_f64().unwrap_or(0.0),
                        "（未给理由——建议改用带 rationale 的对象形态）",
                    ),
                }
            })
            .collect();

        // **归一化**（`shangyi-selection` 的职责，此前从未接线）
        if let Err(e) = shangyi_selection::normalize_selector(&mut scores) {
            return ExecOutcome::Failed(format!(
                "分数归一化失败：{e}。这通常意味着某个分数是非有限值或负数——\
                 判据的原始量纲可以是计数（引擎会归一化），但不能是负数。"
            ));
        }
        if scores.len() != ids.len() {
            return ExecOutcome::Failed(format!(
                "分数个数（{}）与均衡个数（{}）不符。",
                scores.len(),
                ids.len()
            ));
        }
        self.state.selector_scores.insert(sid, scores.clone());
        ExecOutcome::Ok(
            serde_json::json!({
                "selector": sid.as_str(),
                "equilibria": ids,
                "weights": scores.iter().map(|s| s.weight).collect::<Vec<_>>(),
            })
            .to_string(),
        )
    }

    /// `select_fuse`：多判据融合（加权）。
    fn select_fuse(&mut self, _args: &serde_json::Value) -> ExecOutcome {
        // 前置校验（解集、分数）都在 `fuse_with_current_weights` 内——
        // 这里不重复，避免两处校验迟早分歧。
        //
        // 融合逻辑**只有一份**（`fuse_with_current_weights`）：偏好变更后的刷新
        // 与这里的首算必须逐位一致，否则"同一个偏好"在不同时点会给出不同分布。
        if let Err(e) = self.fuse_with_current_weights() {
            return ExecOutcome::Failed(format!("融合失败：{e}"));
        }
        match &self.state.distribution {
            Some(d) => ExecOutcome::Ok(
                serde_json::to_string_pretty(d).unwrap_or_else(|_| "{}".into()),
            ),
            None => ExecOutcome::Failed("融合未产出分布（内部不一致）".into()),
        }
    }

    /// `kb_retrieve`：知识库检索（**LLM-Wiki 导航式**，R22）。
    ///
    /// # 三种用法（对应 wiki 的层次）
    ///
    /// 1. **取索引**（不给参数）：返回目录页 —— 模型先读它定位
    /// 2. **取某页**（给 `page`）：返回该页正文 + 出链（带理由）+ 入链
    /// 3. **关键词**（给 `query`）：兜底，用于"库里有没有提过 X"这类确认
    ///
    /// # 为什么主路径是导航而不是相似度检索
    ///
    /// 领域知识的核心结构是「这条理论在什么条件下失效、该转去哪条」——
    /// 那是**关系**，而相似度检索看不见关系。导航式把关系显式放在出链里，
    /// 并让模型沿图走。附带好处是**可解释**：模型能回答"为什么取到这一页"
    /// （因为从 A 页的第 12 行链接过来），而向量检索只能给一个分数。
    fn kb_retrieve(&self, args: &serde_json::Value) -> ExecOutcome {
        let Some(wiki) = &self.state.wiki else {
            // **「未加载」与「空库」必须区分**：前者是没接上，后者是接了但没内容。
            // 混同会让「知识库没配」表现为「知识库里没有这条理论」——
            // 而后者是一个知识主张，读者会当真。
            return ExecOutcome::Blocked(
                "知识库未加载（SHANGYI_KB_DIR 未设置或加载失败）。\
                 这不是「知识库里没有相关条目」，是**根本没接上**。"
                    .into(),
            );
        };

        // 用法 1：取某页
        if let Some(page_id) = args.get("page").and_then(|v| v.as_str()) {
            return match wiki.render_page(page_id) {
                Some(text) => ExecOutcome::Ok(text),
                None => {
                    // 找不到时**给出相近的 id**：直接报「没有」会让模型反复试错，
                    // 而它本可以一次改对
                    let near = wiki.search(&[page_id.to_string()]);
                    let hint = if near.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "\n库中相近的页：{}",
                            near.iter()
                                .take(5)
                                .map(|(p, _)| p.id.clone())
                                .collect::<Vec<_>>()
                                .join("、")
                        )
                    };
                    ExecOutcome::Failed(format!(
                        "知识库中没有页 `{page_id}`。可用 `kb_retrieve` 不带参数取索引查看全部页。{hint}"
                    ))
                }
            };
        }

        // 用法 3：关键词
        if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
            if !q.trim().is_empty() {
                let keywords: Vec<String> = q
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                    .map(str::to_string)
                    .collect();
                let hits = wiki.search(&keywords);
                if hits.is_empty() {
                    // 关键词无命中时**返回索引**而不是空结果：让模型能改用导航。
                    // 只报「没有」会误导——关键词匹配看不见链接关系，
                    // 「关键词没中」不等于「库里没有相关理论」。
                    return ExecOutcome::Ok(format!(
                        "关键词 `{q}` 在知识库中没有命中。\n\
                         **这不代表库中没有相关理论**——关键词匹配看不见链接关系。\n\
                         以下是索引，供你按图定位：\n\n{}",
                        wiki.render_index()
                    ));
                }
                let mut out = format!("关键词 `{q}` 命中 {} 页：\n\n", hits.len());
                for (p, _s) in hits.iter().take(8) {
                    out.push_str(&format!(
                        "- **{}**（{}）— {}\n",
                        p.id,
                        p.kind.as_str(),
                        p.summary.trim()
                    ));
                }
                out.push_str("\n用 `kb_retrieve` 传 `page` 取某页正文与它的链接。\n");
                return ExecOutcome::Ok(out);
            }
        }

        // 用法 2（默认）：取索引
        let mut out = wiki.render_index();
        if !wiki.is_clean() {
            out.push_str(&format!(
                "\n> ⚠ 本库有 {} 处内容问题。引用时请注意：\
                 **断链不代表这条理论没有后续，可能是页名拼写错误**。\n",
                wiki.issues().len()
            ));
        }
        ExecOutcome::Ok(out)
    }

    /// `monte_carlo`：参数不确定性传播（种子固定 ⇒ 可复现）。
    ///
    /// # ⚠️ 这里曾经有一个死结（run24 实测）——两个接口之间的鸿沟
    ///
    /// 早先本工具要求调用方给 `samples` 数组，注释写着"由上游仿真/引擎产出的
    /// 样本"。**但没有任何工具产出样本**——于是模型拿不到它，只能：
    /// 要么编一组数（违反铁律 1），要么请求回退（而回退需人裁决，自动模式下
    /// 被拒）⇒ 卡死。run24 里模型的诊断完全正确：
    ///
    /// > recommend 阶段的两件核心工作都缺输入：monte_carlo 需要"上游确定性
    /// > 引擎产出的样本"……没有它，我只能编造数值，违反"所有数值必须来自
    /// > 确定性工具"与铁律 2。
    ///
    /// **这不是模型的错，是接口设计的错**：把"引擎该做的计算"分包给了模型。
    ///
    /// # 现在的做法：本工具**自己做**采样与重解
    ///
    /// 它从参数的**已声明分布**（`Param.dist`，其来源必须合法）抽样，
    /// 对每个样本**重解博弈**，再按固定种子汇总分位数。
    /// 随机性只存在于"抽样"这一步，而种子固定 ⇒ 同样输入给出逐位相同的结果。
    ///
    /// # 报什么量（这是唯一诚实的选法）
    ///
    /// 每个样本解出的可能不止一个均衡（而我们**尚未做选择**——那是
    /// select 阶段的职责）。故不能报"某个均衡的支付"——那等于擅自选了。
    ///
    /// 报的是**焦点玩家的支付区间**：对每个样本，取该样本全部均衡里
    /// 焦点支付的**最小值（悲观）与最大值（乐观）**，再对这两条序列各求
    /// P10/P50/P90。语义读作"在参数不确定性下，我方收益的可能范围"——
    /// 不含任何选择判断。
    fn monte_carlo(&self, args: &serde_json::Value) -> ExecOutcome {
        // 兼容旧形态：调用方确实自带样本时（如由外部仿真提供），仍接受。
        if let Some(a) = args.get("samples").and_then(|v| v.as_array()) {
            let samples: Vec<f64> = a.iter().filter_map(|v| v.as_f64()).collect();
            if samples.is_empty() {
                return ExecOutcome::Failed("`samples` 为空，无法计算分位数。".into());
            }
            return match shangyi_consequence::percentiles_from_samples(samples) {
                Ok(p) => ExecOutcome::Ok(
                    serde_json::json!({
                        "mode": "external_samples",
                        "p10": p.p10, "p50": p.p50, "p90": p.p90,
                        "note": "分位数由确定性代码计算；样本来自调用方（非本引擎生成）。",
                    })
                    .to_string(),
                ),
                Err(e) => ExecOutcome::Failed(format!("分位数计算失败：{e}")),
            };
        }

        let Some(spec) = self.state.game_spec.clone() else {
            return ExecOutcome::Failed("会话内尚无 GameSpec，无法做不确定性传播。".into());
        };
        let Some(param_name) = args.get("param").and_then(|v| v.as_str()) else {
            return ExecOutcome::Failed(
                "`monte_carlo` 需要 `param`（要传播不确定性的**参数名**）。\n\
                 分布取自该参数在 GameSpec 里**已声明的 `dist`**——\
                 没有声明分布的参数无法做传播（那是「点估计」，不是「不确定量」）。"
                    .into(),
            );
        };
        let param = match spec.params.get(param_name) {
            Some(p) => p.clone(),
            None => {
                return ExecOutcome::Failed(format!(
                    "参数 `{param_name}` 不在 GameSpec 的参数表内。可用：{}",
                    spec.params.keys().cloned().collect::<Vec<_>>().join("、")
                ))
            }
        };
        let Some(dist) = param.dist.clone() else {
            return ExecOutcome::Failed(format!(
                "参数 `{param_name}` **没有声明分布**（`dist` 缺席）⇒ 它是点估计，\
                 不是不确定量，故「传播不确定性」无从谈起——\
                 本工具不替你假设一个分布（那会造出无来源的不确定性模型，\
                 而报告会把它读成「这个范围有依据」）。\n\
                 ⚠️ **补分布需要回到 `model_build` 阶段**——`gamespec_build` \
                 只在那个阶段可用，而阶段是单向的，本阶段无法补。\n\
                 可选处置：\n\
                 1. 请求回退到 `model_build`（用 `request_rewind`，需用户裁决）\
                 2. **不做传播**，直接进 `recommend_build`——它接受本模式的\
                    其他依据（如 `consequence_predict` 的后果报告），\
                    并在报告里如实标注「未做不确定性传播」。\n\
                 路径 2 不是降级：没有来源的分布本来就不该被编出来。"
            ));
        };
        if matches!(param.source, shangyi_gamespec::ParamSource::Assumed { .. }) {
            // 铁律 2 的类型级强制：Assumed 参数不可用于求解
            return ExecOutcome::Failed(format!(
                "参数 `{param_name}` 的来源是 `assumed`——铁律 2 禁止无来源的数值入模，\
                 故也不能拿它做不确定性传播。请先经 checkpoint 确认。"
            ));
        }

        let draws = args
            .get("draws")
            .and_then(|v| v.as_u64())
            .filter(|n| *n > 0 && *n <= 2000)
            .unwrap_or(200) as u32;
        let seed = args.get("seed").and_then(|v| v.as_u64()).unwrap_or(20260918);

        match self.solve_at_draws(&spec, param_name, &dist, draws, seed) {
            Ok(v) => ExecOutcome::Ok(serde_json::to_string_pretty(&v).unwrap_or_default()),
            Err(e) => ExecOutcome::Failed(e),
        }
    }

    /// 抽样 → 逐样本重解 → 汇总焦点支付区间。
    ///
    /// 抽成独立方法是刻意的：`monte_carlo` 与 `sensitivity_scan` 都要
    /// "改一个参数再重解"，而那条路径**必须只有一份**——两份实现迟早会在
    /// 参数覆盖或求解调用上分歧，而那会让两个工具对同一个博弈给出不同结果。
    fn solve_at_draws(
        &self,
        spec: &shangyi_gamespec::GameSpec,
        param_name: &str,
        dist: &shangyi_gamespec::Dist,
        draws: u32,
        seed: u64,
    ) -> Result<serde_json::Value, String> {
        let focal_idx = focal_player_index(spec);
        let mut rng = shangyi_consequence::Lcg::new(seed);
        let mut mins: Vec<f64> = Vec::with_capacity(draws as usize);
        let mut maxs: Vec<f64> = Vec::with_capacity(draws as usize);
        let mut eq_counts: Vec<f64> = Vec::with_capacity(draws as usize);
        let mut failures = 0u32;
        let mut first_failure: Option<String> = None;

        for _ in 0..draws {
            let x = sample_from_dist(&mut rng, dist);
            let mut s = spec.clone();
            if let Some(p) = s.params.get_mut(param_name) {
                p.value = shangyi_gamespec::Expr::Literal(x);
                // 抽样得到的点是**派生的**，不是原始假设——如实改来源，
                // 否则报告会把它读成"用户确认过的数值"。
                p.source = shangyi_gamespec::ParamSource::Derived {
                    note: format!("monte_carlo 从已声明分布抽样（种子 {seed}）"),
                };
                p.dist = None; // 该样本是点值，不再带分布
            }
            let game = match NormalFormGame::from_gamespec(&s) {
                Ok(g) => g,
                Err(e) => {
                    failures += 1;
                    first_failure.get_or_insert_with(|| e.to_string());
                    continue;
                }
            };
            let sol = match shangyi_solver::NativeSolver.solve(SolveRequest::new(game)) {
                Ok(s2) => s2,
                Err(e) => {
                    failures += 1;
                    first_failure.get_or_insert_with(|| e.to_string());
                    continue;
                }
            };
            // 空解集**不是**"支付为 0"，它是"该样本下无均衡"——如实跳过后计数
            if sol.equilibria.is_empty() {
                failures += 1;
                first_failure.get_or_insert_with(|| "该样本下求解器给出空解集".to_string());
                continue;
            }
            let f: Vec<f64> = sol
                .equilibria
                .iter()
                .map(|e| e.payoffs[focal_idx].to_f64())
                .collect();
            mins.push(f.iter().cloned().fold(f64::INFINITY, f64::min));
            maxs.push(f.iter().cloned().fold(f64::NEG_INFINITY, f64::max));
            eq_counts.push(sol.equilibria.len() as f64);
        }

        if mins.is_empty() {
            return Err(format!(
                "全部 {draws} 个样本都未能求解（首次失败：{}）。\
                 这不代表「结果稳健」——**一个样本都没算出来**。\
                 常见原因：参数取值范围导致支付矩阵不可解，或参数未被任何支付引用。",
                first_failure.unwrap_or_else(|| "(未记录)".into())
            ));
        }

        let pmin = shangyi_consequence::percentiles_from_samples(mins.clone())
            .map_err(|e| format!("悲观序列分位数失败：{e}"))?;
        let pmax = shangyi_consequence::percentiles_from_samples(maxs.clone())
            .map_err(|e| format!("乐观序列分位数失败：{e}"))?;
        let pn = shangyi_consequence::percentiles_from_samples(eq_counts)
            .map_err(|e| format!("均衡数分位数失败：{e}"))?;

        Ok(serde_json::json!({
            "mode": "engine_sampling",
            "param": param_name,
            "draws": draws,
            "seed": seed,
            "solved": mins.len(),
            "failed": failures,
            "pessimistic": {"p10": pmin.p10, "p50": pmin.p50, "p90": pmin.p90},
            "optimistic": {"p10": pmax.p10, "p50": pmax.p50, "p90": pmax.p90},
            "equilibria_count": {"p10": pn.p10, "p50": pn.p50, "p90": pn.p90},
            "_caveats": [
                "每个样本对**全部均衡**取 min/max，**未做选择**——\
                 「哪个均衡更可能」是 select 阶段的职责。故这不是概率预测，\
                 而是「在参数不确定性下我方支付的可能范围」。",
                format!("抽样与求解全部由确定性代码完成，种子 {seed} ⇒ 同输入同结果。"),
                if failures > 0 {
                    format!("有 {failures} 个样本未能求解（已跳过并计数）。\
                             跳过**不是**「结果稳定」——若跳过比例高，本结论的代表性下降。")
                } else {
                    "全部样本均求解成功。".to_string()
                },
                "参数来源已标为 `derived`（抽样点不是原始假设）——\
                 报告引用时不得读成「用户确认过的数值」。".to_string(),
            ],
        }))
    }

    /// `sensitivity_scan`：翻转阈值扫描。
    ///
    /// # ⚠️ 与 `monte_carlo` 同一个死结（run24 实测）
    ///
    /// 早先本工具要求调用方给"沿某参数扫描得到的 (值, 是否翻转) 点列"，
    /// 注释写着"扫描本身由引擎执行"——**但没有工具执行它**。于是模型既拿不到
    /// 输入、又被告知不该自己算 ⇒ 只能请求回退（而自动模式下被拒）⇒ 卡死。
    ///
    /// 现在本工具**自己做扫描**：沿指定参数取值 → 逐点重解 → 判定"推荐是否
    /// 翻转"。
    ///
    /// # "翻转"的判据（这是本工具唯一有解释余地的地方，故写清楚）
    ///
    /// 每一步都可能有多个均衡。**不用"某个均衡的支付"**（那要选一个均衡，
    /// 而选择是 select 阶段的职责）。判据是**均衡集合的结构是否变化**：
    ///
    /// - 记下基准点（参数取当前值）的均衡"签名"：各均衡按支付排序后的形状
    /// - 某点的签名与基准不同 ⇒ 该点**翻转**
    ///
    /// 这个判据的优点是不需要外部选择、且对"均衡个数变化"与"支付结构变化"
    /// 都敏感。它的**局限**如实说明：签名相同不代表策略相同（可能只是支付
    /// 数值巧合地构成同样的排序），故输出里同时给出每点的均衡数与焦点支付，
    /// 让读者能自己核。
    fn sensitivity_scan(&mut self, args: &serde_json::Value) -> ExecOutcome {
        // 兼容旧形态：调用方自带扫描点时仍接受（外部工具做过扫描的场景）
        if let Some(a) = args.get("points").and_then(|v| v.as_array()) {
            let points: Vec<(f64, bool)> = a
                .iter()
                .filter_map(|v| {
                    let x = v.get("param")?.as_f64()?;
                    let flipped = v.get("flipped")?.as_bool()?;
                    Some((x, flipped))
                })
                .collect();
            if points.len() < 2 {
                return ExecOutcome::Failed("`points` 至少需要 2 个点才能定位翻转。".into());
            }
            return ExecOutcome::Ok(render_flip_scan(&points, "external_points"));
        }

        let Some(spec) = self.state.game_spec.clone() else {
            return ExecOutcome::Failed("会话内尚无 GameSpec，无法做敏感性扫描。".into());
        };
        let Some(param_name) = args.get("param").and_then(|v| v.as_str()).map(str::to_string)
        else {
            return ExecOutcome::Failed(
                "`sensitivity_scan` 需要 `param`（要扫描的**参数名**）。\n\
                 扫描范围取自参数当前值与 `steps`：本工具不替你猜范围。"
                    .into(),
            );
        };
        let param = match spec.params.get(&param_name) {
            Some(p) => p.clone(),
            None => {
                return ExecOutcome::Failed(format!(
                    "参数 `{param_name}` 不在 GameSpec 的参数表内。可用：{}",
                    spec.params.keys().cloned().collect::<Vec<_>>().join("、")
                ))
            }
        };
        if !param.source.is_solvable() {
            return ExecOutcome::Failed(format!(
                "参数 `{param_name}` 的来源是 `assumed`——铁律 2 禁止无来源的数值入模，\
                 故也不能扫描它。"
            ));
        }
        // 基准值：取参数当前的字面量（引用型参数无法直接取基准，如实拒绝）
        let base = match &param.value {
            shangyi_gamespec::Expr::Literal(x) => *x,
            shangyi_gamespec::Expr::Ref(r) => {
                return ExecOutcome::Failed(format!(
                    "参数 `{param_name}` 的值是引用 `{r}` 而非字面量——\
                     扫描需要一个可变的数值基准。请先把它解成字面量。"
                ))
            }
        };

        // 扫描范围：显式给，或按基准值的 50%–150% 生成（**如实标注为默认**）
        let (lo, hi) = match (args.get("from").and_then(|v| v.as_f64()), args.get("to").and_then(|v| v.as_f64())) {
            (Some(a), Some(b)) if a < b => (a, b),
            (Some(_), Some(_)) => {
                return ExecOutcome::Failed("`from` 必须小于 `to`。".into())
            }
            _ => {
                // 默认范围是**启发式**：基准值的 ±50%。它会被标注出来，
                // 因为"扫描范围本身"也是一个建模选择，不该被隐藏。
                let span = (base.abs() * 0.5).max(1e-9);
                (base - span, base + span)
            }
        };
        let steps = args
            .get("steps")
            .and_then(|v| v.as_u64())
            .filter(|n| *n >= 2 && *n <= 200)
            .unwrap_or(21) as u32;
        let used_default_range = args.get("from").is_none() || args.get("to").is_none();

        let points: Vec<(f64, bool)> =
            match self.scan_param(&spec, &param_name, lo, hi, steps) {
                Ok(p) => p,
                Err(e) => return ExecOutcome::Failed(e),
            };

        let mut out: serde_json::Value = serde_json::from_str(&render_flip_scan(&points, "engine_scan"))
            .unwrap_or_else(|_| serde_json::json!({}));
        if let Some(o) = out.as_object_mut() {
            o.insert("param".into(), serde_json::json!(param_name));
            o.insert("base_value".into(), serde_json::json!(base));
            o.insert("range".into(), serde_json::json!([lo, hi]));
            o.insert("steps".into(), serde_json::json!(steps));
            o.insert(
                "_caveats".into(),
                serde_json::json!([
                    "翻转判据是**均衡签名**的变化（各均衡按支付排序后的形状），                     不是某个均衡的支付——因为选哪个均衡是 select 阶段的职责。                     签名相同不代表策略相同：输出里同时给了每点的均衡数与焦点支付，请自行核对。",
                    if used_default_range {
                        format!("扫描范围 [{lo}, {hi}] 是**默认启发式**（基准 {base} 的 ±50%）——                                 范围本身是一个建模选择，若结论对范围敏感请显式给 from/to。")
                    } else {
                        "扫描范围由调用方显式指定。".to_string()
                    },
                    "扫描与重解全部由确定性代码完成（无随机性），同输入同结果。".to_string(),
                ]),
            );
        }
        ExecOutcome::Ok(serde_json::to_string_pretty(&out).unwrap_or_default())
    }

    /// 沿参数扫描并逐点重解，返回 `(参数值, 是否翻转)`。
    ///
    /// 与 `solve_at_draws` 是**两条不同的路径**（一个抽样、一个扫描），
    /// 但都走"改一个参数再重解"这个动作。它们共享 `NormalFormGame::from_gamespec`
    /// 与 `NativeSolver.solve`——那是真正的公共路径，故不必再抽一层。
    fn scan_param(
        &self,
        spec: &shangyi_gamespec::GameSpec,
        param_name: &str,
        lo: f64,
        hi: f64,
        steps: u32,
    ) -> Result<Vec<(f64, bool)>, String> {
        // 基准点的签名
        let base_sig = equilibrium_signature(spec, param_name, f64::NAN)?;
        let mut out = Vec::with_capacity(steps as usize);
        for i in 0..steps {
            let t = i as f64 / (steps - 1) as f64;
            let x = lo + (hi - lo) * t;
            let sig = equilibrium_signature(spec, param_name, x)?;
            out.push((x, sig != base_sig));
        }
        Ok(out)
    }

    /// 本模式下、当前状态下可用的**建议依据**。
    ///
    /// 按 `self.mode` 分派，保证 `discover` 在前置 `consequence_predict` 后
    /// 不会被 `consequence_report` 遮蔽；`scenario` 在孤立单测未显式切模式时
    /// 保留已有产物的回退路径。
    fn recommendation_basis(&self) -> Option<RecommendationBasis> {
        match self.mode {
            crate::ConsultationMode::Discover => self
                .state
                .discovery_report
                .as_ref()
                .map(|_| RecommendationBasis::DiscoveryReport),
            crate::ConsultationMode::Whatif => self
                .state
                .consequence_report
                .as_ref()
                .map(|_| RecommendationBasis::ConsequenceReport),
            crate::ConsultationMode::Scenario => {
                if self.state.distribution.is_some() {
                    Some(RecommendationBasis::EquilibriumDistribution)
                } else if self.state.consequence_report.is_some() {
                    Some(RecommendationBasis::ConsequenceReport)
                } else if self.state.discovery_report.is_some() {
                    Some(RecommendationBasis::DiscoveryReport)
                } else {
                    None
                }
            }
        }
    }

    /// 当前时间（Unix 毫秒）。**画像时间戳的唯一来源**。
    ///
    /// 用系统时钟而非可注入时钟：`DomainExecutor` 目前没有时钟字段，
    /// 而画像的 `updated_at_ms` 只用于审计排序（不参与任何计算），
    /// 故不值得为它引入注入点。若将来它参与计算，就必须注入——
    /// 否则确定性重放会失败。
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// 用**当前**权重与已登记的判据分数重新融合（供偏好变更后刷新分布）。    /// 用**当前**权重与已登记的判据分数重新融合（供偏好变更后刷新分布）。
    ///
    /// 抽成独立方法是刻意的：`select_fuse` 与"偏好变更后的刷新"必须走
    /// **同一条融合路径**——两份实现迟早会在归一化或置信度上分歧，
    /// 而那会让"同一个偏好"在不同时点给出不同的分布。
    fn fuse_with_current_weights(&mut self) -> Result<(), String> {
        let ids = self.equilibrium_ids().map_err(|e| match e {
            ExecOutcome::Failed(m) => m,
            _ => "解集不可用".to_string(),
        })?;
        if self.state.selector_scores.is_empty() {
            // 错误信息必须**可操作**：只说"没有分数"会让模型不知道下一步做什么，
            // 而它需要的是"去调 select_equilibria"。
            return Err(
                "尚无任何判据的分数。请先用 `select_equilibria` 为每个判据登记分数。".into(),
            );
        }
        let weights = match &self.state.selector_weights {
            Some(w) => w.clone(),
            None => {
                let ids_in_use: Vec<shangyi_selection::SelectorId> =
                    self.state.selector_scores.keys().copied().collect();
                shangyi_selection::SelectorWeights::uniform(&ids_in_use)
            }
        };
        let confidence = shangyi_selection::Confidence::PriorUncalibrated {
            samples: 0,
            needed: shangyi_selection::MIN_SAMPLES_FOR_CALIBRATION,
        };
        match shangyi_selection::fuse(&ids, &self.state.selector_scores, &weights, confidence) {
            Ok(d) => {
                self.state.distribution = Some(d);
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// `recommend_build`：汇总为建议包（recommend 阶段的交付物）。
    ///
    /// # 它解决的死锁（实测三连卡）
    ///
    /// 阶段图要求 `recommend` 阶段产出 `RecommendationPack` 才能进 `report`。
    /// 但**没有任何工具能产出它**——设计文档 §7.3 的阶段表列了 `recommend_build`，
    /// 而注册表里从未有它。
    ///
    /// 后果：三次端到端真实咨询（run13/14/15）**全部停在 recommend**，
    /// 报告阶段在结构上不可达。而日志里只看到"阶段推进到 recommend"之后
    /// 什么都没有——因为模型每次尝试推进都被闸门拦下。
    ///
    /// # 它不编造结论
    ///
    /// 建议包的**实质内容**（推荐动作、依据）由模型给出——那是它的职责。
    /// 但本工具会**自动收进**已算出的确定性结果（敏感性阈值、偏好设置、
    /// 均衡分布），并检查它们是否存在。模型凭空写一个"建议"而没有任何
    /// 引擎结果支撑时，本工具会指出缺什么。
    fn recommend_build(&mut self, args: &serde_json::Value) -> ExecOutcome {
        // ── 检查不该缺的前置：没有"依据"就"建议"什么？ ──
        //
        // ⚠️ 依据**按模式而异**（这是实测暴露的结构性缺陷）。
        //
        // 早先这里硬性要求 `distribution`（select 阶段的产物）。但
        // **whatif 与 discover 模式的路径里没有 select**（见
        // `PhaseGraph::for_mode`：discover 用 discover 阶段替代 select，
        // whatif 用 consequence）——于是这两个模式的 `recommend` 阶段
        // **结构性不可达**：它们的路径上根本产不出分布。
        //
        // 修法：接受**本模式实际会产出的**那些依据。
        //
        // | 模式 | 依据来源 |
        // |---|---|
        // | scenario | `select_fuse` 的均衡概率分布 |
        // | whatif | `consequence_predict` 的条件后果报告（对手反应概率）|
        // | discover | `discover_satisfice` 的满意策略评估 |
        //
        // 共同点是：**它们都是引擎算出来的**，而不是模型的主张。
        // 这才是这条前置检查的真正目的——防止"建议退化成凭空主张"。
        let basis = self.recommendation_basis();
        if basis.is_none() {
            return ExecOutcome::Failed(
                "尚无「依据」可以支撑建议——需要下列之一（取决于模式）：\n\
                 · scenario：`select_fuse` 产出的均衡概率分布\n\
                 · whatif：`consequence_predict` 产出的条件后果报告\n\
                 · discover：`discover_satisfice` 产出的满意策略评估\n\
                 没有依据的建议会退化成凭空主张，故本工具拒绝。"
                    .into(),
            );
        }
        let basis_kind = basis.map(|b| b.as_str()).unwrap_or("none");

        let mut headline = args
            .get("headline")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim()
            .to_string();
        if headline.is_empty() {
            return ExecOutcome::Failed(
                "`recommend_build` 需要 `headline`（一句话结论，进老板页首段）。".into(),
            );
        }

        let mut actions: Vec<serde_json::Value> = args
            .get("actions")
            .and_then(|v| v.as_array())
            .filter(|a| !a.is_empty())
            .cloned()
            .unwrap_or_default();
        if actions.is_empty() {
            return ExecOutcome::Failed(
                "`recommend_build` 需要至少一条 `actions`——没有行动清单的\
                 「建议包」不是建议。".into(),
            );
        }
        // 每条行动要有依据——否则是凭空主张
        let unsupported: Vec<String> = actions
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                a.get("rationale")
                    .and_then(|r| r.as_str())
                    .map(str::trim)
                    .unwrap_or("")
                    .is_empty()
            })
            .map(|(i, _)| format!("第 {} 条", i + 1))
            .collect();
        if !unsupported.is_empty() {
            return ExecOutcome::Failed(format!(
                "以下行动没有 `rationale`：{}。每条建议都要指向**已算出的结果**——\
                 没有依据的建议无法被审计。",
                unsupported.join("、")
            ));
        }

        // A rationale is not authorization to exceed a real user limit. Bound
        // actions are resolved from stored tool results, never caller attributes.
        if self.state.constraints.has_limits() {
            if self.state.constraints.needs_review() {
                return ExecOutcome::Failed("真实用户底线已新增或修改，须先用 constraint_check 重新核对可接受方案；旧建议和确认不能沿用。".into());
            }
            if args.get("constraint_revision").and_then(|v|v.as_u64()) != Some(self.state.constraints.revision()) {
                return ExecOutcome::Failed("constraint_revision 与当前用户底线版本不一致；请读取 constraint_check 的最新结果后重建建议。".into());
            }
            for action in &mut actions {
                let Some(id)=action.get("scenario_id").and_then(|v|v.as_str()) else {
                    return ExecOutcome::Failed("有用户硬上限时，每条推荐必须绑定已计算的 scenario_id，不能用 action/rationale 文字绕过。".into());
                };
                let Some(scenario)=self.state.cost_scenarios.get(id) else {
                    return ExecOutcome::Failed(format!("推荐引用了不存在的方案 {id}；须先计算并核对实际方案属性。"));
                };
                let attributes:Vec<ConstraintAttribute>=match serde_json::from_value(scenario["constraint_attributes"].clone()) {
                    Ok(attributes)=>attributes,
                    Err(_)=>return ExecOutcome::Failed("方案缺少可校验属性，请重新运行 business_cost；方案名称不证明月租或签约期间。".into()),
                };
                let assessment=self.state.constraints.assess(&attributes);
                if assessment.status!=Acceptance::Eligible {
                    return ExecOutcome::Failed(format!("方案 {id} 不能进入推荐或可接受方案清单：{}。超限报价可以保留作对照，未知属性不能当作符合底线。",serde_json::to_string(&assessment).unwrap()));
                }
                let verified:Vec<String>=attributes.iter().filter_map(|attribute|attribute.value.map(|value| {
                    let currency=match attribute.unit.as_str(){"CNY"|"RMB"|"人民币"|"元"=>"元","USD"=>"美元","EUR"=>"欧元","HKD"=>"港元",other=>other};
                    match attribute.metric {
                        crate::constraints::ConstraintMetric::Rent=>format!("月租{value}{currency}/月"),
                        crate::constraints::ConstraintMetric::UpfrontCash=>format!("开始时现金支出{value}{currency}"),
                        crate::constraints::ConstraintMetric::LeaseDuration=>format!("签约期限{value}个月"),
                    }
                })).collect();
                // The actionable text comes from the verified binding. A model
                // cannot write “accept 12000” while binding a different 11000 plan.
                let rationale="该方案符合你已明确提出的上限；具体条件和费用依据一并保留。";
                *action=serde_json::json!({"action":format!("按以下条件推进：{}",verified.join("；")),
                    "scenario_id":id,"rationale":rationale,"verified_attributes":attributes,"constraint_assessment":assessment});
            }
            headline="以下方案符合你明确提出的上限；超出上限或关键条件仍未知的方案仅作对照。".into();
        }

        let mut do_not: Vec<String> = args
            .get("do_not")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if self.state.constraints.has_limits() {
            do_not=vec!["不执行超过你已声明上限、或关键金额/期限尚未取得的方案。".into()];
        }

        // 自动收进确定性结果（不靠模型转述）
        let dist_summary = self
            .state
            .distribution
            .as_ref()
            .and_then(|d| d.most_likely().map(|(e, ties)| (e.equilibrium_id.clone(), e.prob, ties)));
        let pref = self
            .state
            .game_spec
            .as_ref()
            .and_then(|g| g.preference.as_ref())
            .map(|p| p.risk_aversion);

        // caveats 需在宏外构造（json! 不支持链式迭代器）
        let mut caveats: Vec<String> = Vec::new();
        if pref.is_none() {
            caveats.push(
                "未注入用户风险偏好（`preference_inject` 未调用）——判据权重为剧本先验。"
                    .to_string(),
            );
        }
        if dist_summary.as_ref().map(|(_, _, t)| *t).unwrap_or(false) {
            caveats.push(
                "最高概率的均衡**存在并列**——只报一个会掩盖分歧。".to_string(),
            );
        }
        if self.state.consequence_report.is_none() {
            caveats.push("未做条件后果预测（`consequence_predict` 未调用）。".to_string());
        }

        let pack = serde_json::json!({
            "headline": headline,
            "actions": actions,
            "do_not": do_not,
            "constraint_review":self.state.constraints.snapshot(),
            // ── 以下是**自动收进**的确定性结果（模型不该也不能改）──
            "basis": basis_kind,
            "equilibrium_distribution": {
                "most_likely": dist_summary.as_ref().map(|(id, p, _)| serde_json::json!({
                    "equilibrium": id, "probability": p,
                })),
                "has_tie": dist_summary.as_ref().map(|(_, _, t)| *t).unwrap_or(false),
                "entries": self.state.distribution.as_ref().map(|d| {
                    d.entries.iter().map(|e| serde_json::json!({
                        "equilibrium": e.equilibrium_id, "prob": e.prob,
                    })).collect::<Vec<_>>()
                }).unwrap_or_default(),
                "confidence": self.state.distribution.as_ref().map(|d| d.confidence.note()),
            },
            "risk_aversion": pref,
            "caveats": caveats,
        });

        // 登记产物——**这一步才是解开死锁的关键**
        let summary = serde_json::to_string(&pack).unwrap_or_default();
        self.state.recommendation = Some(summary.clone());
        ExecOutcome::Ok(summary)
    }

    /// `preference_inject`：把用户偏好注入**选择器的权重**（§5.5、§8.2）。
    ///
    /// # 它影响什么（这才是它存在的意义）
    ///
    /// `GameSpec.preference` 的字段（`risk_aversion` / `objective_weights`）
    /// **目前没有任何引擎消费**——它们是设计预留。若本工具只把它们写进去，
    /// 就会造出一个"看起来生效、实际不影响任何结果"的工具——那比不实现更坏：
    /// 用户会以为自己的风险偏好影响了推荐。
    ///
    /// 故本工具做的是**真正影响结果的那件事**：把风险偏好映射为
    /// **选择器权重的调整**。用户越厌恶风险，`risk_dom`（风险占优）的权重越高，
    /// `persona_sim`（LLM 模拟，波动大）的权重越低。
    ///
    /// # 映射规则是**显式的**（不是藏在公式里的魔法）
    ///
    /// `risk_aversion ∈ [0,1]`（0=风险中性，1=极度厌恶）：
    /// - `risk_dom` 权重乘 `(1 + risk_aversion)`
    /// - `persona_sim` 权重乘 `(1 - 0.5 * risk_aversion)`
    /// - 其余不变
    ///
    /// 然后重新归一化。**规则写在这里是因为它是可争议的**——它是一个
    /// 启发式映射，其合理区间应由回测确定（与 `DIVERGENCE_THRESHOLD` 同类）。
    fn preference_inject(&mut self, args: &serde_json::Value) -> ExecOutcome {
        let Some(spec) = self.state.game_spec.as_mut() else {
            return ExecOutcome::Failed("会话内尚无 GameSpec，无法注入偏好。".into());
        };

        // ⚠️ 不能 `unwrap_or(0.0)`：
        //
        // 那会把"给了非法值"（NaN / 字符串 / null）**静默变成"风险中性"**——
        // 用户以为自己注入了偏好，实际得到的是默认值。实测踩到：
        // JSON 里的 NaN 经 `as_f64()` 返回 `None`，于是 `unwrap_or(0.0)` 生效。
        //
        // 正确做法：**给了就必须是合法数**；没给才用默认。
        let risk_aversion = match args.get("risk_aversion") {
            None => 0.0,
            Some(v) => match v.as_f64() {
                Some(x) => x,
                None => {
                    return ExecOutcome::Failed(format!(
                        "`risk_aversion` 不是合法数值：{v}。\
                         （注意：JSON 里的 NaN/Infinity 不是合法字面量，无法解析为数值。）"
                    ));
                }
            },
        };
        if !(0.0..=1.0).contains(&risk_aversion) || !risk_aversion.is_finite() {
            return ExecOutcome::Failed(format!(
                "`risk_aversion` 须在 [0,1]，实得 {risk_aversion}。\
                 （0=风险中性，1=极度厌恶风险）"
            ));
        }

        // 目标权重（可选）：用户对多目标的侧重，如 {"利润":0.7,"份额":0.3}
        let objective_weights: BTreeMap<String, f64> = args
            .get("objective_weights")
            .and_then(|v| v.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
                    .collect()
            })
            .unwrap_or_default();
        if let Some((k, v)) = objective_weights
            .iter()
            .find(|(_, v)| !v.is_finite() || **v < 0.0)
        {
            return ExecOutcome::Failed(format!("目标 `{k}` 的权重非法：{v}（须有限且非负）"));
        }

        // 写进 GameSpec.preference（留档；它的消费方在后续演进中）
        spec.preference = Some(shangyi_gamespec::Preference {
            risk_aversion,
            objective_weights: objective_weights.clone(),
        });

        // ── 真正影响结果的：调整选择器权重 ──
        let before: BTreeMap<String, f64> = self
            .playbook
            .selectors
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        let mut adjusted = before.clone();
        for (k, v) in adjusted.iter_mut() {
            match k.as_str() {
                "risk_dom" => *v *= 1.0 + risk_aversion,
                "persona_sim" => *v *= 1.0 - 0.5 * risk_aversion,
                _ => {}
            }
        }
        let total: f64 = adjusted.values().sum();
        let normalized: BTreeMap<String, f64> = if total > 0.0 {
            adjusted.iter().map(|(k, v)| (k.clone(), v / total)).collect()
        } else {
            adjusted.clone()
        };

        // 用调整后的权重重建 select_fuse 将使用的权重表。
        //
        // ⚠️ **剧本没配选择器先验时，绝不写权重表**（实测踩到）：
        //
        // 早先无条件 `self.state.selector_weights = Some(w)`，而 `w` 在
        // `before` 为空时是**空表**——于是"注入偏好"反而把原本可用的
        // （或 `None` ⇒ 等权基线）权重覆盖成"一个选择器都没有"，
        // 下游融合随即失败。方向完全反了：用户表达偏好，结果是推荐算不出来。
        let mapping = if normalized.is_empty() {
            "unavailable"
        } else {
            let mut weights = BTreeMap::new();
            for (k, v) in &normalized {
                if let Some(id) = shangyi_selection::SelectorId::ALL
                    .iter()
                    .find(|s| s.as_str() == k.as_str())
                {
                    weights.insert(*id, *v);
                }
            }
            if weights.is_empty() {
                // 剧本的选择器名与选择器注册表对不上（拼写错？）——同样是
                // **不写**，并把这件事说出来，而不是静默留个空表。
                "selector_names_unmatched"
            } else {
                match shangyi_selection::SelectorWeights::new(weights) {
                    Ok(w) => {
                        self.state.selector_weights = Some(w);
                        "playbook_priors"
                    }
                    Err(e) => return ExecOutcome::Failed(format!("权重表构造失败：{e}")),
                }
            }
        };

        // 偏好变了 ⇒ 用**新权重重新融合**，而不是把分布清空。
        //
        // # 为什么"清空"是错的（实测死锁）
        //
        // 早先的实现在这里 `self.state.distribution.take()`，理由是
        // "否则下游会读到与当前偏好不符的分布"。那个理由对，但处置错了：
        //
        // - 重建分布的 `select_fuse` **只在 select 阶段可用**（阶段白名单）
        // - 而 `preference_inject` 在 recommend 阶段——**回不去 select**
        // ⇒ 偏好一旦注入，分布就永久消失，`recommend_build` 永远失败
        //   （它要求有分布）。实测 run19 正是如此卡住：模型三次尝试、
        //   正确诊断出"上游没跑过"、请求回退，而自动模式拒绝回退 ⇒ 卡死。
        //
        // 正确处置：**就地用新权重重新融合**。判据分数（`selector_scores`）
        // 还在，融合是纯函数，故这一步是确定性的、不需要回退。
        //
        // 刷新失败时**回滚到刷新前的一致状态**（旧权重配旧分布）：宁可这次偏好
        // 不生效并如实说明，也不能留下"权重已改、分布还是旧的"这种不自洽状态，
        // 更不能留下"分布没了"。回滚后 `needs_select_stage=true` 才是真话——
        // 那种情形下确实只有 select 阶段能重建分布。
        let had = self.state.distribution.is_some();
        let prev_weights = self.state.selector_weights.clone();
        let prev_dist = self.state.distribution.take();
        let (updated, refresh_error) = match self.fuse_with_current_weights() {
            Ok(()) => (true, None),
            Err(e) => {
                self.state.selector_weights = prev_weights;
                self.state.distribution = prev_dist;
                (false, Some(e))
            }
        };

        ExecOutcome::Ok(
            serde_json::json!({
                "risk_aversion": risk_aversion,
                "objective_weights": objective_weights,
                "selector_weights_before": before,
                "selector_weights_after": normalized,
                "weights_mapping": mapping,
                "distribution_refreshed": had && updated,
                "needs_select_stage": !updated,
                "refresh_error": refresh_error,
                "note": "偏好通过**调整选择器权重**影响推荐（不是只写进 GameSpec——                         那两个字段目前无引擎消费）。映射规则：risk_dom ×(1+λ)、                         persona_sim ×(1-0.5λ)，再归一化。该规则是启发式的，                         其合理区间应由案例库回测确定。",
            })
            .to_string(),
        )
    }

    /// `sandbox_exec`：临时数值代码沙箱（**探索性**，产出不得进报告）。
    ///
    /// # 唯一用途
    ///
    /// 执行模型**临时写的**数值代码（试算、画图、探索性拟合）。
    /// 核心求解器**不经过**这里——均衡、选择器、蒙特卡洛全是原生 Rust
    /// 在主进程内确定性执行（设计文档 §5.3 明确）。
    ///
    /// # 产出为什么不得进报告
    ///
    /// 工具注册表把它标为 [`TrustLevel::Exploratory`]，报告生成器拒绝读取。
    /// 返回体恒带 `exploratory: true` 作为下游的红灯。
    /// 若某个探索结果确实要进报告，正确路径是**用 Rust 重写那段计算**——
    /// 数字的可信性来自"代码是我们自己写的、可复现的"，
    /// 不来自"它在沙箱里跑过"。
    ///
    /// # fail-closed
    ///
    /// 无法隔离时返回 `Blocked`（**能力缺失**），绝不无隔离地跑。
    /// 且该状态与"代码本身出错"在类型上分开——否则模型会去改一段
    /// 本来没错的代码。
    fn sandbox_exec(&mut self, args: &serde_json::Value) -> ExecOutcome {
        let Some(code) = args.get("code").and_then(|v| v.as_str()) else {
            return ExecOutcome::Failed("`sandbox_exec` 需要 `code`（Python 代码）。".into());
        };
        if code.trim().is_empty() {
            return ExecOutcome::Failed("`code` 为空。".into());
        }

        let mut cfg = shangyi_core_sandbox::SandboxConfig::default();
        if let Some(t) = args.get("timeout_secs").and_then(|v| v.as_u64()) {
            // 上限同样是 300 秒：探索性任务不该占满一次咨询的时间预算。
            if t == 0 || t > 300 {
                return ExecOutcome::Failed(format!(
                    "`timeout_secs` 须在 1–300 之间，实得 {t}。"
                ));
            }
            cfg.timeout_secs = t;
        }

        match shangyi_core_sandbox::run(code, &cfg) {
            Ok(out) => match serde_json::to_string_pretty(&out) {
                Ok(j) => ExecOutcome::Ok(j),
                Err(e) => ExecOutcome::Failed(format!("结果序列化失败：{e}")),
            },
            // **能力缺失** ⇒ Blocked（不是脚本错误，别去改代码）
            Err(shangyi_core_sandbox::SandboxError::CannotIsolate { reason }) => {
                ExecOutcome::Blocked(format!("无法隔离执行：{reason}"))
            }
            // 静态校验 / 超时 ⇒ Failed（这些是**代码的问题**，改代码有用）
            Err(e) => ExecOutcome::Failed(format!("{e}")),
        }
    }

    /// `data_ingest`：CSV/表格解析（**进程隔离的 Python 侧车**）。
    ///
    /// # 它做什么、不做什么（可信度分级 `Sidecar` 的落点）
    ///
    /// 做：把文件解析成结构化数据 + **列语义建议**（带依据）。
    /// 不做：任何推断性计算（不算增长率、不算均值、不猜缺失值）。
    ///
    /// 这条边界有硬性含义：侧车的输出是**数据**，不是结论。
    /// 报告里若引用它，须标注"来自用户提供的文件"，而不是"引擎算出"。
    ///
    /// # 为什么"编码探测失败"必须报错而不是硬读
    ///
    /// 中文中小企业场景里 GBK/GB18030 很常见。若探测失败还按 UTF-8 硬读，
    /// 会得到**乱码列名**——而列名会被下游当成真实字段名用于建模。
    /// 乱码列名比"读不出来"危险得多：后者会停下来，前者会一路流进报告。
    fn data_ingest(&mut self, args: &serde_json::Value) -> ExecOutcome {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return ExecOutcome::Failed(
                "`data_ingest` 需要 `path`（文件路径）。本工具不搜索文件——\
                 路径必须由调用方给出，避免「猜一个文件」这种不可审计的行为。"
                    .into(),
            );
        };
        if path.trim().is_empty() {
            return ExecOutcome::Failed("`path` 为空。".into());
        }

        let mut req = shangyi_intel::sidecar::IngestRequest::new(path);
        if let Some(n) = args.get("max_rows").and_then(|v| v.as_u64()) {
            req = req.with_max_rows(n as usize);
        }
        if let Some(e) = args.get("encoding").and_then(|v| v.as_str()) {
            req = req.with_encoding(e);
        }
        if let Some(d) = args.get("delimiter").and_then(|v| v.as_str()) {
            req = req.with_delimiter(d);
        }

        let client = shangyi_intel::sidecar::IngestClient::from_env();
        match client.ingest(&req) {
            Ok(shangyi_intel::sidecar::IngestOutcome::Parsed(report)) => {
                // 存进状态：`intel` 阶段的"情报摘要"可以引用它
                self.state.ingested = self
                    .state
                    .ingested
                    .iter()
                    .cloned()
                    .chain(std::iter::once((*report).clone()))
                    .collect();
                match serde_json::to_string_pretty(&*report) {
                    Ok(j) => ExecOutcome::Ok(j),
                    Err(e) => ExecOutcome::Failed(format!("解析报告序列化失败：{e}")),
                }
            }
            // `Refused` 是**侧车明确拒绝**（编码探测不出、格式不支持等）——
            // 与"侧车没跑起来"是两件事，分开报告。
            Ok(shangyi_intel::sidecar::IngestOutcome::Refused(r)) => ExecOutcome::Failed(
                format!(
                    "侧车拒绝解析该文件（这是**明确的拒绝**，不是崩溃）：{}\n\
                     → 若你知道编码，可用 `encoding` 参数显式指定。",
                    // 侧车的拒绝类型没有 `Serialize`（它的字段是给 Rust 用的）。
                    // 用 `Debug` 输出——信息完整，只是不那么好看。
                    format!("{r:?}")
                ),
            ),
            // 侧车本身不可用（解释器缺失、脚本缺失、超时）——这是**能力 seam**，
            // 必须 `Blocked` 而非 `Failed`：后者暗示"文件有问题"，
            // 而真相是"我们这边没接上"。
            Err(e) => ExecOutcome::Blocked(format!(
                "解析侧车不可用：{e}\n\
                 这不是「文件有问题」，是**解析能力没接上**。"
            )),
        }
    }

    /// `swarm_run`：persona 群仿真（§5.7）——**行为证据与候选生成，不是结论**。
    ///
    /// # 它产出什么、不产出什么（这条边界是本工具的核心）
    ///
    /// - 产出：各 persona 在 rollout 里的**行动计数**（行为证据），
    ///   以及经引擎三重过滤的**候选评估**（矩阵外行动能否进博弈）
    /// - 不产出：任何概率分布、任何"因此应该怎么做"的结论
    ///
    /// 铁律 1 的落点：sub-agent 可以提议行动（含矩阵外），**评估一律回主引擎**
    /// ——即 [`shangyi_swarm::orchestrator::evaluate_candidates`]。
    /// 本执行器**不自己判定**某个候选是否有效。
    ///
    /// # 为什么必须检查 `trace`（否则这里会静默降级）
    ///
    /// [`shangyi_swarm::PersonaActor::act`] 返回 `Option<Move>`，于是
    /// 「该 persona 这轮没想法」与「LLM 调用失败」在编排器眼里**完全一样**，
    /// 都会让 rollout 以 `StopReason::Stable` 收尾。
    ///
    /// 若本执行器直接把这个 Stable 当证据交出去，一次**端点故障**就会以
    /// 「博弈已稳定」的面目进报告——而报告读者不会知道那些"稳定"的结论
    /// 其实来自一台没回话的服务器。
    ///
    /// 故：**先看 trace**。全部失败 ⇒ `Failed`（这不是"没有证据"，是"取证据失败"），
    /// 部分失败 ⇒ 如实标注在输出里。
    fn swarm_run(&mut self, args: &serde_json::Value) -> ExecOutcome {
        let Some(spec) = self.state.game_spec.clone() else {
            return ExecOutcome::Failed("会话内尚无 GameSpec，无法跑 persona 仿真。".into());
        };
        let Some(factory) = &self.swarm_actor else {
            // **没有 LLM 时不退回"规则驱动"**：那会让"没接上"表现为
            // "仿真结果如此"。类型化的 Blocked 才是真话。
            return ExecOutcome::Blocked(
                "persona 群仿真需要 LLM 客户端，但本次会话未装配（`swarm_run` \
                 依赖 CLI 组装层注入 actor）。这不是「仿真没有进展」，是**没接上**。"
                    .into(),
            );
        };
        if self.state.profiles.is_empty() {
            return ExecOutcome::Failed(
                "尚无参与者画像。persona 由画像生成——没有画像就没有 persona，\
                 也就没有可仿真的行为主体。请先调用 `participant_profile_update`。"
                    .into(),
            );
        }

        let rollouts = args
            .get("rollouts")
            .and_then(|v| v.as_u64())
            .filter(|n| *n > 0 && *n <= 64)
            .unwrap_or(8) as u32;
        let seed = args.get("seed").and_then(|v| v.as_u64()).unwrap_or(20260918);

        // persona 由**画像**生成（不是模型编的角色卡）：画像里的
        // decision_style / interests 是用户在 intake 给的或情报里取得的，
        // 它们才是"这个对手是什么人"的依据。
        let cards: Vec<(String, String, String)> = spec
            .players
            .iter()
            .filter_map(|p| {
                self.state
                    .profiles
                    .iter()
                    .find(|pr| pr.participant_id == p.id)
                    .map(|pr| {
                        (
                            p.id.clone(),
                            p.name.clone(),
                            describe_profile_for_persona(pr),
                        )
                    })
            })
            .collect();
        if cards.is_empty() {
            return ExecOutcome::Failed(
                "画像与博弈参与者对不上号：GameSpec 里的玩家没有任何一个有对应画像。\
                 请先为参与者登记画像（`participant_profile_update`）。"
                    .into(),
            );
        }

        let personas = shangyi_swarm::PersonaSet::new(
            cards
                .iter()
                .map(|(id, name, card)| shangyi_swarm::Persona {
                    participant_id: id.clone(),
                    display_name: name.clone(),
                    decision_style: card.lines().next().unwrap_or("").to_string(),
                    role_card: card.clone(),
                    tool_allowlist: vec![],
                })
                .collect(),
        );

        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("(未指定)")
            .to_string();
        // 同一份 personas 既给 rollout、也给 actor——见 `SwarmActorFactory` 的文档。
        let actor = factory.build(&model, personas.clone());

        let config = shangyi_swarm::orchestrator::OrchestratorConfig::default();
        let trajectories = match shangyi_swarm::orchestrator::rollout_n(
            &spec,
            &personas,
            actor.as_ref(),
            &config,
            rollouts,
            seed,
        ) {
            Ok(t) => t,
            Err(e) => return ExecOutcome::Failed(format!("rollout 失败：{e}")),
        };

        // ── **先看 trace**（见方法文档：不看它则故障会伪装成"稳定"）──
        let trace = actor.trace();
        if trace.total_failure() {
            return ExecOutcome::Failed(format!(
                "persona 仿真**一次提议都没产出**（尝试 {} 次，全部失败）。\
                 这不是「仿真显示行为稳定」，是**取证据失败**——LLM 端点不可用、\
                 或响应无法解析为行动。最后失败原因：{}",
                trace.attempts,
                trace.last_failure.as_deref().unwrap_or("(未记录)")
            ));
        }

        let tally = shangyi_swarm::tally_actions(&trajectories);
        // 三重过滤的阈值：设计文档 §5.7.1 要求它们由回测确定；
        // 冷启动期用**显式的默认值**（可被调用方覆盖），且报告须标注未校准。
        let filter = shangyi_consequence::TripleFilter::default();
        let candidates =
            match shangyi_swarm::orchestrator::evaluate_candidates(&spec, &trajectories, &filter) {
                Ok(c) => c,
                Err(e) => return ExecOutcome::Failed(format!("候选评估失败：{e}")),
            };

        let stop_reasons: Vec<String> = trajectories
            .iter()
            .map(|t| format!("{:?}", t.stop_reason))
            .collect();
        let all_stable = trajectories
            .iter()
            .all(|t| t.stop_reason == shangyi_swarm::StopReason::Stable);

        // ⚠️ **工具输出必须最小充分**——它会被后续每一步重发。
        //
        // # 实测（一次真实咨询的成本剖析）
        //
        // 本工具的输出曾是 **55 KB**，占该次咨询全部工具输出的 **44%**——
        // 而它出现在一个 turn 的中段，于是**后面每一步都要重发这 55 KB**
        // （实测同 turn 内输入从 2k 涨到 58k token）。
        //
        // 拆开看，55 KB 里 47% 是 `candidates`（约 143 条，每条把
        // `actor`/`is_novel` 等字段名重复一遍）、32% 是 `action_tally`
        // （同样的平铺结构）。
        //
        // # 修法：模型要的与报告要的**不是同一份东西**
        //
        // - **模型需要**：persona 做了什么（频次表）、矩阵外提议的裁决
        //   （那是它唯一无法从别处知道的信息）、以及如实标注
        // - **报告需要**：全量细节（"考虑过但排除了哪些路径"——
        //   §5.6.1 的透明性要求）
        //
        // 后者留在 `state.swarm_output`（报告读它），不必进模型上下文。
        // 矩阵内动作更是**不必逐条列出**——策略空间已在 GameSpec 里。
        //
        // 这不是"为了省钱而藏信息"：模型仍拿到它行动所需的全部依据，
        // 而完整细节在事件日志与 state 里**可审计**（不变量 ③ 满足）。
        let tally_nested: BTreeMap<&str, BTreeMap<String, u32>> = {
            let mut m: BTreeMap<&str, BTreeMap<String, u32>> = BTreeMap::new();
            for ((actor_id, action, novel), n) in &tally {
                // 矩阵外动作加标记，使模型能分辨"策略空间内的选择"与"提议的新动作"
                let key = if *novel {
                    format!("[矩阵外] {action}")
                } else {
                    action.clone()
                };
                m.entry(actor_id.as_str()).or_default().insert(key, *n);
            }
            m
        };
        let novel_verdicts: Vec<serde_json::Value> = candidates
            .entries
            .iter()
            .filter(|e| e.is_novel || !e.rejection.is_empty())
            .map(|e| {
                serde_json::json!({
                    "actor": e.actor,
                    "action": e.action,
                    "accepted": e.engine_accepted,
                    "rejection": e.rejection,
                })
            })
            .collect();

        // ── 把**被引擎接受**的矩阵外动作注册进博弈 ──
        //
        // ⚠️ 这一步此前**从未接线**（`register_accepted_novel_moves` 只在
        // swarm crate 的测试里被调用）。后果：§5.7.1 的"非常规行动可进博弈"
        // 这条链在真实运行中**断了**——persona 提议、引擎评估、然后……
        // 结果就停在那里，没有变成 `GameSpec` 里的策略。
        //
        // 那正是"提议 ≠ 结论"的**另一半**：提议经评估后应当**生效**。
        // 若只评估不注册，`novel_move_filter` 与 persona 仿真就都成了
        // "展示了可能性但没用上"——而报告会引用它们，读者以为它们已被采纳。
        //
        // 注册是**改 GameSpec 的状态变更**，故必须在评估之后、且如实计数。
        let mut spec_after = spec.clone();
        let registered = shangyi_swarm::orchestrator::register_accepted_novel_moves(
            &mut spec_after,
            &candidates,
            &trajectories,
            &filter,
        );
        // ⚠️ **必须写回状态**——否则上面那一步是空操作。
        //
        // 我第一版算完 `spec_after` 就丢掉了（只把计数放进输出），
        // 于是"注册"变成了"报告说注册了几个、实际博弈没变"——
        // 那比不注册更坏：报告会声称非常规行动已被采纳，
        // 而它们不在任何后续求解里。
        //
        // 只有真的写回，后续的 `solve_equilibria` 才会把新策略纳入矩阵
        // （这也意味着注册后应**重新求解**——指引里说明了这点）。
        if registered > 0 {
            self.state.game_spec = Some(spec_after);
        }

        // 全量细节留给报告（不进模型上下文）
        let full = serde_json::json!({
            "rollouts": rollouts,
            "seed": seed,
            "trajectories": trajectories.len(),
            "action_tally": tally
                .iter()
                .map(|((actor_id, action, novel), n)| serde_json::json!({
                    "actor": actor_id,
                    "action": action,
                    "matrix_external": novel,
                    "count": n,
                }))
                .collect::<Vec<_>>(),
            "candidates": candidates,
            "stop_reasons": stop_reasons,
            "_trace": {
                "attempts": trace.attempts,
                "proposed": trace.proposed,
                "rejected": trace.rejected,
                "novels_missing_ranges": trace.novels_missing_ranges,
                "last_failure": trace.last_failure,
            },
            "registered_novel_moves": registered,
            "_caveats": build_swarm_caveats(&trace, all_stable),
        });
        self.state.swarm_output = Some(full.clone());

        // 给模型的**紧凑版**（报告用全量，见 `state.swarm_output`）
        let out = serde_json::json!({
            "rollouts": rollouts,
            "seed": seed,
            "trajectories": trajectories.len(),
            "action_tally": tally_nested,
            "novel_move_verdicts": novel_verdicts,
            "counts": {
                "matrix_internal_actions": tally.iter().filter(|((_, _, nov), _)| !*nov).count(),
                "novel_actions": tally.iter().filter(|((_, _, nov), _)| *nov).count(),
                "candidates_evaluated": candidates.entries.len(),
                // 已注册进博弈的矩阵外动作数——**这是"提议生效"的证据**，
                // 而不只是"提议被评估过"。
                "novel_moves_registered": registered,
            },
            "_trace": {
                "attempts": trace.attempts,
                "proposed": trace.proposed,
                "rejected": trace.rejected,
                "novels_missing_ranges": trace.novels_missing_ranges,
                "last_failure": trace.last_failure,
            },
            "_caveats": build_swarm_caveats(&trace, all_stable),
            "_full_detail": "完整细节（含全部候选与逐轨迹的停止原因）存在会话状态里，\
                             报告层会读取；此处只给行动依据。",
        });
        ExecOutcome::Ok(
            serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()),
        )
    }

    /// `consequence_predict`：条件后果预测（whatif 管线，§5.6.1）。
    ///
    /// # 语义（这是它唯一诚实的做法）
    ///
    /// 「如果我方执行动作 d，对手会怎么反应、各方结果如何」——确定性的做法是：
    ///
    /// 1. 把我方**固定在 d 上**（概率 1，因为我们问的是"做了 d 会怎样"）
    /// 2. 在这个前提下构造**对手的残余子博弈**：对手的支付取自原博弈里
    ///    我方 = d 的那一层
    /// 3. 求解残余子博弈 ⇒ 每个均衡是一个「反应」
    /// 4. `P(r|d)` = 该反应的概率
    ///
    /// # 三处必须如实标注的地方（都不允许编造）
    ///
    /// 1. **多均衡时的概率**：残余子博弈若有多个均衡，用等权（并标注未校准）。
    ///    不假装有实证依据——真依据来自案例库回测，而冷启动期没有。
    /// 2. **结果分布**：只有参数声明了 `dist` 才能给 P10/P90。
    ///    未声明时如实报"点估计"（P10=P50=P90），而不是伪造一个区间。
    /// 3. **d 的来源**：d 必须是我方**已声明的策略**。若模型想推演一个
    ///    矩阵外的动作，那要先过 `novel_move_filter` 注册进博弈——
    ///    直接推演一个未注册的动作等于算了一个不存在的博弈。
    fn consequence_predict(&mut self, args: &serde_json::Value) -> ExecOutcome {
        use shangyi_gamespec::RelationToGame;
        use shangyi_solver::{NormalFormGame, SolveRequest, SolverBackend};

        let Some(spec) = self.state.game_spec.clone() else {
            return ExecOutcome::Failed(
                "会话内尚无 GameSpec。条件后果预测需要先在 `model_build` 建立博弈。".into(),
            );
        };
        let Some(d) = args.get("decision").and_then(|v| v.as_str()) else {
            return ExecOutcome::Failed(
                "`consequence_predict` 需要 `decision`（我方要推演的动作）。".into(),
            );
        };

        // 我方 = vn_role 为 focal 且进矩阵的玩家
        let Some(focal_id) = spec
            .players
            .iter()
            .find(|p| {
                p.vn_role == shangyi_gamespec::ValueNetRole::Focal
                    && p.relation_to_game == RelationToGame::Player
            })
            .map(|p| p.id.clone())
        else {
            return ExecOutcome::Failed(
                "GameSpec 里没有 `vn_role: focal` 且 `relation_to_game: player` 的玩家——\
                 无法确定「我方」是谁，故无法推演「我方做了 d」。".into(),
            );
        };

        // d 必须是我方已声明的策略
        let Some(focal_strategies) = spec.strategies.get(&focal_id) else {
            return ExecOutcome::Failed(format!("我方 `{focal_id}` 没有声明策略集。"));
        };
        // ⚠️ 策略名匹配要**宽容**——但不能猜。
        //
        // # 实测（run25：连续失败 3 次）
        //
        // 模型给的 `decision` 是自然语言描述：
        // 「进入新城区，开设 1 家社区生鲜门店（单店投入 80–120 万），常规节奏落地」
        // 而博弈里的策略名是「进入」/「不进入」。它每次都拿到"不在已声明策略里"
        // 并附上可用清单（那条提示是对的、可操作的），但它**改不对**——
        // 因为它想要的语义（带细节的进入方案）在策略空间里根本没有对应的名字。
        //
        // 于是三次调用全部浪费。而正确的处置不是让模型去猜措辞，而是：
        //
        // 1. **先做严格匹配**（名字完全相同）
        // 2. 再做**唯一子串匹配**：若某个策略名是 `decision` 的子串，
        //    且**只有它一个**匹配 ⇒ 采用（这是确定性的，不是猜）
        // 3. 多个策略名都匹配、或一个都不匹配 ⇒ **如实拒绝**并说明歧义
        //
        // 第 2 步的边界很关键：只在**唯一**时采用。若有「进入」与「不进入」
        // 两个策略名，而 `decision` 里"进入"两字都出现（"不进入"含"进入"），
        // 那就**必须拒绝**——静默选一个会让推演的是另一个动作，
        // 而结果看起来完全正常。
        let exact = focal_strategies.iter().position(|s| s == d);
        let d_idx = match exact {
            Some(i) => i,
            None => {
                let hits: Vec<usize> = focal_strategies
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| !s.is_empty() && d.contains(s.as_str()))
                    .map(|(i, _)| i)
                    .collect();
                match hits.len() {
                    0 => {
                        return ExecOutcome::Failed(format!(
                            "动作 `{d}` 不在我方已声明的策略里，且没有任何策略名是它的子串。\n\
                             可用：{}。\n\
                             （若要推演矩阵外的动作，先用 `novel_move_filter` 把它注册进博弈——\
                             直接推演一个未注册的动作等于算了一个不存在的博弈。\n\
                             若你只是想让描述与策略对上，请把 `decision` 直接写成策略名。）",
                            focal_strategies.join(" / ")
                        ))
                    }
                    1 => hits[0],
                    _ => {
                        // 多个候选 ⇒ 拒绝。**不选"最像的"**——那需要相似度
                        // 度量，而任何度量都可能在有歧义时给出一个看似合理的错答案。
                        let names: Vec<String> = hits
                            .iter()
                            .map(|i| focal_strategies[*i].clone())
                            .collect();
                        return ExecOutcome::Failed(format!(
                            "动作 `{d}` 与**多个**已声明策略匹配（{}）——歧义，无法确定你要推演哪一个。\n\
                             请把 `decision` 直接写成策略名（二者选一）：{}。\n\
                             本工具**不猜**：猜错会让你看到的后果属于另一个动作，\
                             而输出看起来完全正常。",
                            names.join("、"),
                            focal_strategies.join(" / ")
                        ));
                    }
                }
            }
        };

        // 原博弈（用于取支付）
        let game = match NormalFormGame::from_gamespec(&spec) {
            Ok(g) => g,
            Err(e) => return ExecOutcome::Failed(format!("GameSpec 无法适配：{e}")),
        };
        let players: Vec<String> = spec
            .players
            .iter()
            .filter(|p| p.relation_to_game == RelationToGame::Player)
            .map(|p| p.id.clone())
            .collect();
        let focal_pos = match players.iter().position(|p| *p == focal_id) {
            Some(i) => i,
            None => return ExecOutcome::Failed("我方不在矩阵玩家列表中".into()),
        };
        let opponents: Vec<String> = players
            .iter()
            .filter(|p| **p != focal_id)
            .cloned()
            .collect();

        // ── 残余子博弈：对手的支付取自"我方 = d"的那一层 ──
        let opp_counts: Vec<usize> = opponents
            .iter()
            .map(|id| spec.strategies.get(id).map(|v| v.len()).unwrap_or(0))
            .collect();
        if opp_counts.contains(&0) {
            return ExecOutcome::Failed("有对手没有声明策略集".into());
        }

        // # 单人对手的情形（2 人博弈固定我方后）必须单独处理
        //
        // `NormalFormGame::new` 要求 2..=4 个玩家，而"我方固定 + 一个对手"
        // 只剩 1 个玩家——那时**没有"求解博弈"可言**：对手的最佳反应
        // 就是在其策略里选支付最高的那个（纯策略，除非并列）。
        //
        // 早先的实现在这里报错（"玩家数 1 不在 2..=4"），使 2 人博弈的
        // whatif 完全不可用——而 2 人正是最常见的场景。
        if opponents.len() == 1 {
            let opp_id = &opponents[0];
            let opp_pos = players.iter().position(|p| p == opp_id).unwrap_or(0);
            let n_strat = opp_counts[0];
            let mut best: Vec<(usize, shangyi_solver::Rational)> = Vec::new();
            for s_idx in 0..n_strat {
                let mut full = vec![0usize; players.len()];
                full[focal_pos] = d_idx;
                full[opp_pos] = s_idx;
                let v = game.payoff(opp_pos, &full);
                best.push((s_idx, v));
            }
            let max = best
                .iter()
                .map(|(_, v)| *v)
                .reduce(|a, b| if b > a { b } else { a })
                .expect("opp_counts > 0 checked above");
            let winners: Vec<usize> = best
                .iter()
                .filter(|(_, v)| *v == max)
                .map(|(i, _)| *i)
                .collect();
            let names = spec.strategies.get(opp_id).cloned().unwrap_or_default();
            let prob = 1.0 / winners.len() as f64;
            let mut reactions = Vec::with_capacity(winners.len());
            for s_idx in &winners {
                let mut full = vec![0usize; players.len()];
                full[focal_pos] = d_idx;
                full[opp_pos] = *s_idx;
                let mut outcomes = BTreeMap::new();
                for (p, id) in players.iter().enumerate() {
                    let v = game.payoff(p, &full).to_f64();
                    outcomes.insert(
                        id.clone(),
                        shangyi_consequence::Percentiles { p10: v, p50: v, p90: v },
                    );
                }
                reactions.push(shangyi_consequence::ReactionOutcome {
                    reaction: format!(
                        "{}:{}",
                        opp_id,
                        names.get(*s_idx).cloned().unwrap_or_default()
                    ),
                    probability: prob,
                    outcomes,
                    drivers: vec![format!(
                        "我方固定为 `{d}` 时，对手的最大支付为 {}",
                        max
                    )],
                    is_novel: false,
                });
            }
            let note = format!(
                "我方固定为 `{d}`，对手有 {} 个并列最佳反应（等权 {}）。\
                 结果为**点估计**——参数未声明分布时无从给区间。",
                winners.len(),
                if winners.len() > 1 { "（并列）" } else { "" }
            );
            let report = shangyi_consequence::ConsequenceReport {
                decision: d.to_string(),
                reactions,
                rejected_novel_moves: Vec::new(),
                note,
            };
            let json = match serde_json::to_string_pretty(&report) {
                Ok(j) => j,
                Err(e) => return ExecOutcome::Failed(format!("序列化失败：{e}")),
            };
            self.state.consequence_report = Some(report);
            return ExecOutcome::Ok(json);
        }

        let mut residual = match NormalFormGame::new(opp_counts.clone()) {
            Ok(g) => g,
            Err(e) => {
                return ExecOutcome::Failed(format!(
                    "无法构造残余子博弈（对手数 {}）：{e}",
                    opponents.len()
                ))
            }
        };
        // 枚举对手的全部策略组合，从原博弈取"我方=d 时"的支付
        let opp_profiles: Vec<Vec<usize>> = {
            let total: usize = opp_counts.iter().product();
            (0..total)
                .map(|i| {
                    let mut idx = i;
                    let mut prof = vec![0usize; opp_counts.len()];
                    for k in (0..opp_counts.len()).rev() {
                        prof[k] = idx % opp_counts[k];
                        idx /= opp_counts[k];
                    }
                    prof
                })
                .collect()
        };
        for prof in &opp_profiles {
            // 把对手的 profile 放回原博弈的坐标（我方位置固定为 d_idx）
            let mut full = vec![0usize; players.len()];
            for (k, opp_pos) in opponents
                .iter()
                .enumerate()
                .map(|(k, id)| (k, players.iter().position(|p| p == id).unwrap_or(0)))
            {
                full[opp_pos] = prof[k];
            }
            full[focal_pos] = d_idx;
            for (op, id) in opponents.iter().enumerate() {
                let orig_pos = players.iter().position(|p| p == id).unwrap_or(0);
                residual.set_payoff(op, prof, game.payoff(orig_pos, &full));
            }
        }

        // ── 求解残余子博弈 ──
        let sol = match shangyi_solver::NativeSolver.solve(SolveRequest::new(residual.clone())) {
            Ok(s) => s,
            Err(e) => {
                return ExecOutcome::Failed(format!(
                    "残余子博弈求解失败：{e}\n\
                     （这表示「对手在我方执行 {d} 后会怎样」这一步算不出来——\
                     不是「对手没有反应」。）"
                ))
            }
        };
        if sol.equilibria.is_empty() {
            return ExecOutcome::Failed(
                "残余子博弈没有均衡——这不常见，请检查博弈定义。".into(),
            );
        }

        // ── 组装反应 ──
        // 概率：多均衡时**等权**，并如实标注未校准。
        let n = sol.equilibria.len() as f64;
        let equal_weight = 1.0 / n;
        let mut reactions = Vec::with_capacity(sol.equilibria.len());
        for eq in &sol.equilibria {
            // 反应的文字表示：对手各自的策略名
            let mut reaction = String::new();
            for (op, id) in opponents.iter().enumerate() {
                let names = spec.strategies.get(id).cloned().unwrap_or_default();
                let sup: Vec<String> = eq.support[op]
                    .iter()
                    .filter_map(|s| names.get(*s).cloned())
                    .collect();
                if !reaction.is_empty() {
                    reaction.push_str(" + ");
                }
                reaction.push_str(&format!("{}:{}", id, sup.join("/")));
            }
            // 各方结果：**点估计**（按混合均衡策略分布精确计算全员期望支付，含我方与所有对手）
            let mut sigma: Vec<Vec<shangyi_solver::Rational>> = (0..players.len())
                .map(|p| vec![shangyi_solver::Rational::ZERO; game.strategy_count(p)])
                .collect();
            sigma[focal_pos][d_idx] = shangyi_solver::Rational::ONE;
            for (op, oid) in opponents.iter().enumerate() {
                let p_pos = players.iter().position(|p| p == oid).expect("opponent in players");
                sigma[p_pos] = eq.strategy[op].clone();
            }
            let expected = shangyi_solver::verify::expected_payoffs(&game, &sigma);
            let mut outcomes = BTreeMap::new();
            for (p, id) in players.iter().enumerate() {
                let v = expected[p].to_f64();
                outcomes.insert(
                    id.clone(),
                    shangyi_consequence::Percentiles {
                        p10: v,
                        p50: v,
                        p90: v,
                    },
                );
            }
            reactions.push(shangyi_consequence::ReactionOutcome {
                reaction,
                probability: equal_weight,
                outcomes,
                drivers: vec![format!(
                    "残余子博弈的{}均衡（支持枚举，{}）",
                    if eq.kind == shangyi_solver::EqKind::Pure { "纯策略" } else { "混合" },
                    format!("{:?}", sol.completeness)
                )],
                is_novel: false,
            });
        }

        let declared_dists = spec.params.values().filter(|p| p.dist.is_some()).count();
        let note = if declared_dists == 0 {
            "⚠ 结果列为**点估计**（P10=P50=P90）：本博弈的参数未声明分布（`dist`），\
             故无从做 Monte Carlo 区间。要得到区间请在参数上声明 `dist`。\
             反应概率为**等权**（残余子博弈的多均衡之间），未校准——\
             真依据应来自案例库回测，而冷启动期没有。"
                .to_string()
        } else {
            format!(
                "本博弈有 {declared_dists} 个参数声明了分布，但本工具当前按点估计输出\
                 （区间需经 `monte_carlo` 显式传播）。反应概率为等权，未校准。"
            )
        };

        let report = shangyi_consequence::ConsequenceReport {
            decision: d.to_string(),
            reactions,
            rejected_novel_moves: Vec::new(),
            note,
        };
        if !report.probabilities_normalized() {
            return ExecOutcome::Failed("反应概率未归一化（内部错误）".into());
        }

        let json = match serde_json::to_string_pretty(&report) {
            Ok(j) => j,
            Err(e) => return ExecOutcome::Failed(format!("序列化失败：{e}")),
        };
        self.state.consequence_report = Some(report);
        ExecOutcome::Ok(json)
    }

    /// `novel_move_filter`：非常规行动三重过滤。
    ///
    /// # 为什么必须"注册回博弈"而不是只给一个判定
    ///
    /// 反击之所以致命，往往正是因为它**改变了其他人的支付结构**（客户施压 →
    /// 对手的声誉激励改变）。若只算提议者自身收益而不把行动与引入的参与者
    /// 注册回 `GameSpec`，就丢掉了这个机制——星宇事件里学生自己的收益
    /// （出了一口气）远小于公司的损失（市值蒸发），真正的作用链条是
    /// "借第三方之力改变对手的支付"。
    ///
    /// 故本工具**改写会话内的 GameSpec**（注册通过者的外部参与者与新策略），
    /// 调用方随后必须**重解**——注册改变了博弈，旧解集已作废。
    fn novel_move_filter(&mut self, args: &serde_json::Value) -> ExecOutcome {
        let Some(mut spec) = self.state.game_spec.clone() else {
            return ExecOutcome::Failed(
                "会话内尚无 GameSpec。三重过滤要把通过的提议**注册回博弈**，                 故必须先有博弈定义。"
                    .into(),
            );
        };

        let threshold = self
            .playbook
            .selectors
            .get("incentive_threshold")
            .copied()
            .unwrap_or(shangyi_consequence::TripleFilter::default().incentive_threshold);
        let filter = match shangyi_consequence::TripleFilter::new(threshold) {
            Ok(f) => f,
            Err(e) => return ExecOutcome::Failed(format!("过滤阈值非法：{e}")),
        };

        // 参数形态：`{"proposals": [...]}`，或直接给单个提议对象
        let proposals: Vec<shangyi_consequence::NovelMoveProposal> =
            match args.get("proposals") {
                Some(v) => match serde_json::from_value(v.clone()) {
                    Ok(p) => p,
                    Err(e) => return ExecOutcome::Failed(format!("提议列表解析失败：{e}")),
                },
                None => match serde_json::from_value(args.clone()) {
                    Ok(p) => vec![p],
                    Err(e) => {
                        return ExecOutcome::Failed(format!(
                            "非常规行动提议解析失败：{e}。需要字段见 NovelMoveProposal \
                             （actor_id/description/payoff_channel/expected_benefit/expected_cost/…）。"
                        ));
                    }
                },
            };
        if proposals.is_empty() {
            return ExecOutcome::Failed("`proposals` 为空。".into());
        }

        let outcome = shangyi_consequence::filter_and_register(&mut spec, &proposals, &filter);

        let accepted: Vec<serde_json::Value> = outcome
            .accepted
            .iter()
            .map(|m| {
                serde_json::json!({
                    "actor_id": m.proposal.actor_id,
                    "description": m.proposal.description,
                    "incentive_probability": m.verdict.incentive_probability,
                })
            })
            .collect();
        let rejected: Vec<serde_json::Value> = outcome
            .rejected
            .iter()
            .map(|(p, v)| {
                serde_json::json!({
                    "actor_id": p.actor_id,
                    "description": p.description,
                    "legal": v.legal,
                    "feasible": v.feasible,
                    "incentive_compatible": v.incentive_compatible,
                    "incentive_probability": v.incentive_probability,
                    "rejections": v.rejections,
                })
            })
            .collect();

        // 注册改变了博弈 ⇒ 旧解集作废（否则下游会读到与当前模型不符的均衡）
        let had_equilibria = self.state.equilibria_json.take().is_some();
        self.state.game_spec = Some(spec);
        self.state.selector_scores.clear();

        ExecOutcome::Ok(
            serde_json::json!({
                "accepted": accepted,
                "rejected": rejected,
                "threshold": threshold,
                "invalidated_equilibria": had_equilibria,
                "note": if had_equilibria {
                    "有提议被注册回博弈，旧解集已作废——必须重新求解后再选择。"
                } else {
                    "无提议被采纳，博弈未改变。"
                },
            })
            .to_string(),
        )
    }

    /// `discover_satisfice`：满意化策略发现。
    fn discover_satisfice(&mut self, args: &serde_json::Value) -> ExecOutcome {
        // # ⚠️ 输入形态是**候选 + 抱负水平**，不是一个现成的 DiscoveryReport
        //
        // 早先的实现接受 `DiscoveryReport` 的 JSON 并原样返回——那有两个问题：
        //
        // 1. **违反铁律 1**：报告本该由确定性代码**算出**，而那份实现让
        //    调用方（= 模型）直接给出"哪些策略满意"的结论。模型可以编。
        // 2. **产物登记不上**：它只返回字符串而不存状态，于是
        //    `discover` 阶段的 `DiscoveryReport` 产物永远登记不了 ⇒
        //    `discover → recommend` 结构上不可达。
        //
        // 正确做法：接收**候选**（各准则的后果分位数，来自 consequence 管线）
        // 与**抱负水平**（用户给的标准），调 `shangyi_discover::discover()`
        // **计算**出报告。判定与排序全在引擎里。
        let candidates: Vec<shangyi_discover::Candidate> =
            match args.get("candidates").cloned() {
                Some(v) => match serde_json::from_value(v) {
                    Ok(c) => c,
                    Err(e) => {
                        return ExecOutcome::Failed(format!(
                            "`candidates` 解析失败：{e}。\n\
                             每个候选需要：id、description、outcomes（准则名 → P10/P50/P90）、\
                             llm_proposed。outcomes 须由 `consequence_predict` 等\
                             确定性管线产出。"
                        ))
                    }
                },
                None => {
                    return ExecOutcome::Failed(
                        "`discover_satisfice` 需要 `candidates`（要评估的候选策略）与 \
                         `aspiration`（抱负水平）。\n\
                         注意：它**不接受**现成的结论——满意与否由引擎计算。"
                            .into(),
                    )
                }
            };

        let aspiration: shangyi_discover::Aspiration =
            match args.get("aspiration").cloned() {
                Some(v) => match serde_json::from_value(v) {
                    Ok(a) => a,
                    Err(e) => {
                        return ExecOutcome::Failed(format!(
                            "`aspiration` 解析失败：{e}。\n\
                             需要：levels（准则名 → 抱负下限）、thresholds（可选）、\
                             default_threshold（默认 0.6）。"
                        ))
                    }
                },
                None => {
                    return ExecOutcome::Failed(
                        "`discover_satisfice` 需要 `aspiration`（满意标准）——\
                         没有标准的「满意化」没有意义。".into(),
                    )
                }
            };

        match shangyi_discover::discover(&candidates, &aspiration) {
            Ok(report) => {
                // **存状态**：`artifacts()` 从这里登记 DiscoveryReport 产物
                let json = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into());
                let satisfied_n = report.satisfied.len();
                let relax_n = report.relaxation.len();
                self.state.discovery_report = Some(report);
                ExecOutcome::Ok(
                    serde_json::json!({
                        "satisfied_count": satisfied_n,
                        "relaxation_proposals": relax_n,
                        "report": serde_json::from_str::<serde_json::Value>(&json).unwrap_or_default(),
                        "note": if satisfied_n == 0 {
                            "**没有候选达标**——报告里给出的是放宽建议（不伪装成「找到了」）。"
                        } else {
                            "满意策略由引擎判定（不是模型自述）。"
                        },
                    })
                    .to_string(),
                )
            }
            Err(e) => ExecOutcome::Failed(format!("满意化发现失败：{e}")),
        }
    }

    /// `vn_map`：价值网五向扫描。
    ///
    /// # 它做什么、不做什么
    ///
    /// **做**：按剧本声明的优先级与深度，产出五个方向的**扫描清单**，
    /// 并把已收集的画像映射到各方向上报出**覆盖情况**。这是确定性的：
    /// 输入（剧本 + 已有画像）相同 ⇒ 输出相同。
    ///
    /// **不做**：它**不发现**参与者。参与者来自用户回答与公开检索——
    /// 一个"自动推断出你还有哪些对手"的工具只能靠编造。故它的产出是
    /// **"该找什么 + 找到了什么"**，不是"有哪些参与者"。
    ///
    /// 这个区分是必要的：把清单冒充成发现，会让后续建模建立在一个
    /// 未经验证的参与者集合上，而用户无从察觉。
    fn vn_map(&self, args: &serde_json::Value) -> ExecOutcome {
        use shangyi_gamespec::ValueNetRole;

        // 五向 + 外部（§5 的价值网：外部力量只在其被引入时展开）
        let directions = [
            ("substitutor", ValueNetRole::Substitutor, "替代者", "抢同一块需求的直接对手"),
            ("customer", ValueNetRole::Customer, "顾客", "需求与支付意愿的来源"),
            ("supplier", ValueNetRole::Supplier, "供应商", "投入品与议价对象"),
            ("complementor", ValueNetRole::Complementor, "互补者", "让本企业产品更有价值的一方"),
        ];

        // 剧本的扫描优先级与深度（未配置时给出中性默认，并**如实标注**）
        let spec = self.playbook.value_net.as_ref();
        let priority: Vec<String> = spec
            .map(|v| v.priority.clone())
            .unwrap_or_default();
        let depth_of = |key: &str| -> &'static str {
            match spec.and_then(|v| v.scan_depth.get(key)) {
                Some(shangyi_playbooks::ScanDepth::Deep) => "deep",
                Some(shangyi_playbooks::ScanDepth::Medium) => "medium",
                Some(shangyi_playbooks::ScanDepth::Shallow) => "shallow",
                Some(shangyi_playbooks::ScanDepth::Skip) => "skip",
                // 剧本未声明该方向的深度时**如实说明**：不假装已配置
                None => "未配置",
            }
        };
        let rank = |key: &str| -> usize {
            priority.iter().position(|p| p == key).unwrap_or(usize::MAX)
        };

        // 覆盖情况：已收集的画像按角色归类
        let mut by_role: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for p in &self.state.profiles {
            let role = format!("{:?}", p.value_net_role).to_lowercase();
            by_role
                .entry(role)
                .or_default()
                .push(p.participant_id.clone());
        }

        let mut items: Vec<serde_json::Value> = directions
            .iter()
            .map(|(key, _role, label, why)| {
                let found = by_role
                    .get(&key.to_string())
                    .cloned()
                    .unwrap_or_default();
                serde_json::json!({
                    "direction": key,
                    "role_label": label,
                    "why_it_matters": why,
                    "scan_depth": depth_of(key),
                    "priority_rank": rank(key),
                    "profiles_found": found,
                    "covered": !by_role.get(&key.to_string()).map(|v| v.is_empty()).unwrap_or(true),
                })
            })
            .collect();
        // 按剧本优先级排序（未列出者排在后面，保持稳定）
        items.sort_by_key(|it| {
            it["priority_rank"].as_u64().unwrap_or(u64::MAX)
        });

        let covered = items.iter().filter(|i| i["covered"] == true).count();
        let focal = by_role.get("focal").cloned().unwrap_or_default();

        // 外部的引入条件（§5.7.2）：只声明"何时该引入"，不臆断有哪些
        let external_note = args
            .get("introduce_external")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        ExecOutcome::Ok(
            serde_json::json!({
                "focal": focal,
                "directions": items,
                "coverage": format!("{covered}/{}", directions.len()),
                "external_actors": {
                    "note": if external_note {
                        "用户或检索已引入外部力量（监管/舆论/平台），请用 `participant_profile_update` \
                         以 substitutor/customer 之外的角色登记，并在建模时放入 external_actors。"
                    } else {
                        "尚未引入外部力量。价值网五向不含“外部”，它只在被显式引入时展开（§5.7.2）。"
                    },
                },
                "scan_plan_note": "本工具产出的是**扫描清单与覆盖情况**，不是参与者名单。\
                   参与者必须来自用户回答或带来源的公开检索——请勿把清单当作已有发现。",
                "priority_order": priority,
            })
            .to_string(),
        )
    }

    /// `ask_user`：引导式 intake 提问（§8.1）。
    ///
    /// 问题来源：优先用调用方显式给的 `questions`；未给时**从剧本的 intake 清单取**。
    /// 剧本的清单是领域专家写的，比模型临场编问题更可靠——故它是默认来源。
    fn business_cost(&mut self, args: &serde_json::Value) -> ExecOutcome {
        let mut input = args.clone();
        if !input.is_object() {
            return ExecOutcome::Failed("费用参数须为对象".into());
        }
        let identifier = match args.get("operation").and_then(|v| v.as_str()) {
            Some("create") => {
                if args.get("scenario_id").is_some() {
                    return ExecOutcome::Failed("创建新方案时不要指定 scenario_id，由引擎返回标识；改名或调整原方案请用 operation=update 和原标识".into());
                }
                format!("cost-{}", self.state.cost_scenarios.len() + 1)
            }
            Some("update") => match args.get("scenario_id").and_then(|v| v.as_str()) {
                Some(id) if self.state.cost_scenarios.contains_key(id) => id.to_owned(),
                _ => return ExecOutcome::Failed(format!("更新必须引用已存在的 scenario_id，不能为改名另造标识；当前方案：{}",
                    serde_json::to_string(&self.state.cost_scenarios.iter().map(|(id,v)| serde_json::json!({"scenario_id":id,"scenario":v["scenario"]})).collect::<Vec<_>>()).unwrap())),
            },
            _ => return ExecOutcome::Failed("须明确 operation=create（新方案）或 update（已有方案）；改名、补充费用或改期间用 update，不创建第二个方案".into()),
        };
        input["scenario_id"] = serde_json::json!(identifier);
        match crate::business::business_cost(&input) {
            Ok(mut result) => {
                if args["operation"] == "create" {
                    // Equal cost inputs are a reason to ask for an explicit
                    // distinction, not proof that two real-world options match.
                    fn signature(value: &serde_json::Value) -> serde_json::Value {
                        let mut costs:Vec<String> = value["items"].as_array().unwrap().iter().map(|item| {
                            let original=&item["original"];
                            serde_json::json!([item["treatment"],original["amount"],original["period"],
                                original.get("area_sqm"),original.get("billing_days_per_month")]).to_string()
                        }).collect();
                        costs.sort();
                        serde_json::json!([value["currency"],costs])
                    }
                    let candidate=signature(&result);
                    let duplicates:Vec<_>=self.state.cost_scenarios.iter().filter(|(_,v)|signature(v)==candidate)
                        .map(|(id,v)|serde_json::json!({"scenario_id":id,"scenario":v["scenario"]})).collect();
                    if !duplicates.is_empty() {
                        match args.get("distinct_reason").and_then(|v|v.as_str()).filter(|s|!s.trim().is_empty()) {
                            Some(reason) => result["distinct_reason"] = serde_json::json!(reason),
                            None => return ExecOutcome::Failed(format!("相同费用已计算：{}。若是原方案改名、重算或改期间，请 operation=update 并引用其 scenario_id；若确属不同备选对象，保留 create 并给 distinct_reason 说明区别。不会按金额相同自动合并。",serde_json::to_string(&duplicates).unwrap())),
                        }
                    }
                }
                if let Some(reason) = self.state.cost_scenarios.get(&identifier).and_then(|v| v.get("distinct_reason")) {
                    result["distinct_reason"] = reason.clone();
                }
                result["operation"] = args["operation"].clone();
                let attributes:Vec<ConstraintAttribute>=serde_json::from_value(result["constraint_attributes"].clone())
                    .expect("business_cost returns typed attributes");
                let assessment=self.state.constraints.assess(&attributes);
                result["comparison_only"]=serde_json::json!(assessment.status!=Acceptance::Eligible);
                result["constraint_assessment"]=serde_json::json!(assessment);
                result["constraint_review"]=self.state.constraints.snapshot();
                if self.state.constraints.has_limits() && self.state.cost_scenarios.get(&identifier)
                    .is_some_and(|previous|previous.get("constraint_attributes")!=result.get("constraint_attributes")) {
                    self.invalidate_constraint_dependents();
                }
                self.state.cost_scenarios.insert(identifier, result.clone());
                ExecOutcome::Ok(result.to_string())
            }
            Err(error) => ExecOutcome::Failed(error),
        }
    }

    fn constraint_register(&self,args:&serde_json::Value)->ExecOutcome {
        let Some(object)=args.as_object() else {return ExecOutcome::Failed("约束登记参数须为对象".into());};
        if object.keys().any(|key|key!="quote") {
            return ExecOutcome::Failed("constraint_register 只接受真实用户上限原句 quote；不接受模型自填主体/上限数值或 source=user 声明。".into());
        }
        let Some(quote)=args.get("quote").and_then(|v|v.as_str()).filter(|s|!s.trim().is_empty()) else {
            return ExecOutcome::Failed("quote 必须逐字引用用户完整上限语句，不能只摘金额或拼接报价。".into());
        };
        match self.state.constraints.confirm_quote(quote) {
            Ok(registered)=>ExecOutcome::Ok(registered.to_string()),
            Err(reason)=>ExecOutcome::Failed(reason),
        }
    }

    fn constraint_check(&mut self,args:&serde_json::Value)->ExecOutcome {
        let identifiers:Vec<String>=match args.get("scenario_ids") {
            None=>self.state.cost_scenarios.keys().cloned().collect(),
            Some(value)=>match value.as_array().filter(|items|!items.is_empty()) {
                Some(items)=>{
                    let mut ids=Vec::new();
                    for item in items {
                        let Some(id)=item.as_str().filter(|id|self.state.cost_scenarios.contains_key(*id)) else {
                            return ExecOutcome::Failed("scenario_ids 必须引用已计算方案，不能虚构方案或属性。".into());
                        };ids.push(id.to_string());
                    }ids
                }
                None=>return ExecOutcome::Failed("scenario_ids 须为非空标识数组，或省略以核对全部已计算方案。".into()),
            },
        };
        let mut checks=Vec::new();let mut eligible=Vec::new();let mut comparison=Vec::new();let mut unknown=Vec::new();
        for id in identifiers {
            let scenario=self.state.cost_scenarios.get_mut(&id).expect("identifier checked");
            let attributes:Vec<ConstraintAttribute>=match serde_json::from_value(scenario["constraint_attributes"].clone()) {
                Ok(attributes)=>attributes,
                Err(error)=>return ExecOutcome::Failed(format!("方案 {id} 的约束属性无法读取：{error}；请重新计算，不能按空属性默认为可接受。")),
            };
            let assessment=self.state.constraints.assess(&attributes);
            match assessment.status {
                Acceptance::Eligible=>eligible.push(id.clone()),
                Acceptance::Ineligible=>comparison.push(id.clone()),
                Acceptance::Unknown=>unknown.push(id.clone()),
            }
            scenario["comparison_only"]=serde_json::json!(assessment.status!=Acceptance::Eligible);
            scenario["constraint_assessment"]=serde_json::json!(assessment);
            checks.push(serde_json::json!({"scenario_id":id,"assessment":assessment}));
        }
        self.state.constraints.mark_reviewed();
        let current_review=self.state.constraints.snapshot();
        for check in &checks {
            let id=check["scenario_id"].as_str().expect("checked scenario identity");
            self.state.cost_scenarios.get_mut(id).expect("checked scenario exists")["constraint_review"]=current_review.clone();
        }
        ExecOutcome::Ok(serde_json::json!({"constraint_revision":self.state.constraints.revision(),
            "constraints":self.state.constraints.snapshot(),"checks":checks,
            "eligible_scenario_ids":eligible,"comparison_only_scenario_ids":comparison,"unknown_scenario_ids":unknown,
            "note":"可接受仅指已满足当前用户明示上限；未知不等于允许，也不证明方案收益或对方会接受。"}).to_string())
    }

    fn ask_user(&mut self, args: &serde_json::Value) -> ExecOutcome {
        // 1) 显式问题
        if let Some(qs) = args.get("questions").and_then(|v| v.as_array()) {
            if qs.len() > 2 {
                return ExecOutcome::Failed("每轮最多两个问题。请先搜索公开事实，仅筛选必须由用户提供的关键内部信息；不要拆成连续问卷。".into());
            }
            let mut out = Vec::new();
            for (i, q) in qs.iter().enumerate() {
                let text = q.get("text").and_then(|v| v.as_str()).unwrap_or_default();
                if text.trim().is_empty() {
                    return ExecOutcome::Failed(format!("第 {} 个问题缺 `text`。", i + 1));
                }
                out.push(UserQuestion {
                    id: q
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("q{}", i + 1)),
                    text: text.to_string(),
                    kind: q
                        .get("kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("text")
                        .to_string(),
                    options: q
                        .get("options")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                });
            }
            if out.is_empty() {
                return ExecOutcome::Failed("`questions` 为空——没有要问的内容。".into());
            }
            return self.admit_questions(out);
        }

        // 2) 从剧本取
        if args.get("from_playbook").and_then(|v| v.as_bool()).unwrap_or(false)
            || args.get("questions").is_none()
        {
            if self.playbook.intake.is_empty() {
                return ExecOutcome::Failed(format!(
                    "剧本 `{}` 没有声明 intake 问题，且本次调用未给 `questions`。\
                     请显式给出要问的问题。",
                    self.playbook.meta.id
                ));
            }
            // 可选：只问尚未回答过的（由 `skip_answered` 控制）
            let skip_answered = args
                .get("skip_answered")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let qs: Vec<UserQuestion> = self
                .playbook
                .intake
                .iter()
                .filter(|q| !skip_answered || !self.state.answered_intake.contains(&q.id))
                .map(|q| UserQuestion {
                    id: q.id.clone(),
                    text: q.text.clone(),
                    kind: q.kind.clone(),
                    options: q.options.clone(),
                })
                .collect();
            if qs.is_empty() {
                return ExecOutcome::Ok(
                    serde_json::json!({
                        "status": "all_answered",
                        "note": "剧本的 intake 问题已全部回答过。",
                    })
                    .to_string(),
                );
            }
            if qs.len() > 2 {
                return ExecOutcome::Failed("不要直接发送整份剧本清单。请先搜索公开事实，再通过 questions 显式选择最多两个必须由用户回答的问题。".into());
            }
            return self.admit_questions(qs);
        }

        ExecOutcome::Failed("`ask_user` 需要 `questions` 数组，或 `from_playbook: true`。".into())
    }

    fn admit_questions(&mut self, questions: Vec<UserQuestion>) -> ExecOutcome {
        if questions.iter().any(|q| q.text.chars().count()>80 || q.text.matches(['？','?']).count()>1) {
            return ExecOutcome::Failed("每个问题最多80字且只问一件事；不要在一题内罗列成本、面积、报价、押付等多个信息点。先选择当前最影响判断的一项。".into());
        }
        let keys: Vec<String> = questions.iter().map(|q| q.text.chars()
            .filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()).collect();
        if keys.iter().any(|key| self.state.asked_question_texts.contains(key)) {
            return ExecOutcome::Failed("该问题已经提出。请利用已有回答；用户不知道或未回答时保留未知，不要重复催问。确有新事实时说明变化并针对新问题询问。".into());
        }
        self.state.asked_question_texts.extend(keys);
        ExecOutcome::AsksUser(questions)
    }

    /// 记录某个 intake 问题已回答（供 `skip_answered` 用）。
    pub fn mark_intake_answered(&mut self, id: &str) {
        self.state.answered_intake.insert(id.to_string());
    }

    /// `checkpoint`：向用户确认本阶段的事实/假设/结果（§7.6，**铁律 2 的落点**）。
    ///
    /// # 它做什么
    ///
    /// 把"要确认的清单"包装成 [`CheckpointRequest`] 并以
    /// [`ExecOutcome::NeedsUserInput`] 交回循环；循环负责在阶段机上**真正打开**它
    /// （`PhaseMachine::open_checkpoint`）并挂起 turn。
    ///
    /// # 为什么"该打开哪个 checkpoint"由**系统**决定，而不是模型
    ///
    /// 模型只说"我要确认这些内容"，**不指定** cp1/cp2/cp3——
    /// 那是当前阶段决定的（intel⇒cp1、model_build⇒cp2、cp3 按模式落在
    /// select/consequence/discover）。若让模型指定，它就能在 intel 阶段
    /// 打开 cp3 并把结果预览的确认顺手做掉——用户确认的对象与阶段脱钩。
    ///
    /// # 铁律 2 的强制点
    ///
    /// cp2（硬假设确认）的每一项**必须带来源**，否则 [`CheckpointRequest::validate`]
    /// 拒绝。这不是格式检查：一条没有来源的假设即使被用户点了确认，
    /// 确认的也是一个凭空的值——那等于把铁律 2 降级成一句愿望。
    fn checkpoint(&self, args: &serde_json::Value, phase: SessionPhase) -> ExecOutcome {
        // 1) 当前阶段有没有 checkpoint（唯一权威：PhaseGraph）
        let graph = crate::phase_graph::PhaseGraph::for_mode(self.mode);
        let Some(id) = graph.checkpoint_at(phase) else {
            let with_cp: Vec<&str> = graph
                .path()
                .iter()
                .filter(|p| graph.checkpoint_at(**p).is_some())
                .map(|p| p.as_str())
                .collect();
            return ExecOutcome::Failed(format!(
                "阶段 `{}` 没有需要用户确认的检查点。本模式（{}）的确认点在：{}。\
                 请在到达那些阶段时再调用本工具。",
                phase.as_str(),
                self.mode.as_str(),
                with_cp.join("、")
            ));
        };

        // 2) 解析清单
        let raw_items = match args.get("items").and_then(|v| v.as_array()) {
            Some(a) => a,
            None => {
                return ExecOutcome::Failed(
                    "`checkpoint` 需要 `items` 数组，每项含 `content`（要确认的内容）\
                     与 `source`（依据）。缺少清单的确认请求是空操作。"
                        .into(),
                )
            }
        };
        let mut items = Vec::with_capacity(raw_items.len());
        for (i, it) in raw_items.iter().enumerate() {
            let content = it
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if content.trim().is_empty() {
                return ExecOutcome::Failed(format!("第 {} 项缺 `content`。", i + 1));
            }
            items.push(crate::phase_graph::CheckpointItem {
                id: it
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("item{}", i + 1)),
                content: content.to_string(),
                source: it.get("source").and_then(|v| v.as_str()).map(str::to_string),
                proposed_value: it
                    .get("proposed_value")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            });
        }

        // 3) 构造并**就地校验**（铁律 2 的强制点在这里生效，
        //    而不是等到阶段机——尽早失败，错误信息才贴近模型刚做的事）
        let req = crate::phase_graph::CheckpointRequest {
            id,
            phase,
            items,
            revision_requested: false,
        };
        if let Err(e) = req.validate() {
            return ExecOutcome::Failed(format!(
                "{e}。\n\
                 （本阶段需确认的是「{}」。硬假设的每一项都必须给出依据——\
                 没有来源的数值不得进入模型。）",
                id.kind_str_zh()
            ));
        }

        ExecOutcome::NeedsUserInput(req)
    }

    /// `request_rewind`：模型**提议**回退到更早阶段（决定权在用户）。
    ///
    /// # 为什么模型只能提议
    ///
    /// 设计文档 §7.5 规定回退的触发者是**用户**（`shangyi resume --reopen cp2`）。
    /// 理由不只是权限：回退会**作废已完成的工作**，那是有代价的决定，
    /// 用户必须知情并同意。
    ///
    /// # 实测暴露的缺口（本工具存在的原因）
    ///
    /// 一次真实咨询走到 `recommend`（求解、选择、敏感性都完成），
    /// 模型判断"几个硬假设缺来源、结论不可靠"，于是**建议回 model_build**。
    /// 但它既没有工具表达这个判断、也没有权限执行回退——
    /// 结果它把话说完就**停在那里**：没进 report，也没告诉用户怎么继续。
    ///
    /// 给了这个工具之后，它能把判断变成**一个待用户裁决的请求**，
    /// 而不是一句无人接收的建议。
    fn request_rewind(&self, args: &serde_json::Value, phase: SessionPhase) -> ExecOutcome {
        let target_str = args.get("target").and_then(|v| v.as_str()).unwrap_or("");
        let reason = args.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        if reason.trim().is_empty() {
            return ExecOutcome::Failed(
                "`request_rewind` 需要 `reason`——用户要据此判断是否同意作废已完成的工作。"
                    .into(),
            );
        }
        let Some(target) = crate::phase_graph::SessionPhase::parse(target_str) else {
            return ExecOutcome::Failed(format!(
                "未知阶段 `{target_str}`。可用：intake / intel / model_build / solve / select \
                 / consequence / discover / recommend / report / archived"
            ));
        };
        // 只允许**向后**回退；向前或原地没有意义
        if !self.mode_graph().is_earlier(target, phase) {
            return ExecOutcome::Failed(format!(
                "`{target_str}` 不在当前阶段 `{}` 之前——回退只能指向更早的阶段。\
                 （若要跳过当前阶段继续，那不是回退，应直接完成本阶段。）",
                phase.as_str()
            ));
        }
        ExecOutcome::RewindRequest {
            target,
            reason: reason.to_string(),
        }
    }

    /// 本模式的阶段图（供回退合法性判断）。
    fn mode_graph(&self) -> crate::phase_graph::PhaseGraph {
        crate::phase_graph::PhaseGraph::for_mode(self.mode)
    }

    /// `pars_check`：PARTS 检查表的**确定性**完备性判定。
    ///
    /// # 它判定什么、不判定什么
    ///
    /// **判定**：七个要素里哪些**已有依据**（参与者、附加值、规则、战术、范围
    /// 各自是否有画像/参数/剧本声明支撑）。输出是**缺口清单**。
    ///
    /// **不判定**："这个博弈该用哪个模型"——那是 §5.1 的模型映射，不是检查表。
    /// 把两件事混在一起会让检查表变成"模型推荐器"，而它本该只回答"材料够不够"。
    ///
    /// 为什么用 PARTS：它是**策略博弈的完备性检查**（Brandenburger & Nalebuff）。
    /// 缺任一要素，建模时就会把某个真实约束漏在矩阵外——而矩阵外的东西
    /// 不会让结果"看起来错"，只会让它**悄悄不对**。
    fn pars_check(&self, _args: &serde_json::Value) -> ExecOutcome {
        let spec = &self.playbook.value_net;

        // 参与者：画像或 GameSpec 里已有玩家
        let players_from_spec = self
            .state
            .game_spec
            .as_ref()
            .map(|g| {
                g.players
                    .iter()
                    .filter(|p| p.relation_to_game == shangyi_gamespec::RelationToGame::Player)
                    .count()
            })
            .unwrap_or(0);
        let has_players = !self.state.profiles.is_empty() || players_from_spec >= 2;

        // 附加值：等价于"是否有可比较的支付结构"——没有支付就算不出附加值
        let has_added_value = self
            .state
            .game_spec
            .as_ref()
            .map(|g| !g.params.is_empty() || !g.strategies.is_empty())
            .unwrap_or(false);

        // 规则：定义在规则分片上的合规性——商奕里由剧本声明 + 合法性依据承载。
        // 我们不臆测规则，只看**有没有人提供过依据**。
        let has_rules = self
            .state
            .profiles
            .iter()
            .any(|p| !p.timeline.is_empty());

        // 战术：模型是否已建立
        let has_tactics = self.state.game_spec.is_some();

        // 范围：价值网扫描深度是否声明（未声明 = 范围未知）
        let has_scope = spec
            .as_ref()
            .map(|v| !v.scan_depth.is_empty() || !v.priority.is_empty())
            .unwrap_or(false);

        let parts = serde_json::json!({
            "players": {
                "present": has_players,
                "evidence": format!("画像 {} 个，GameSpec 玩家 {} 个",
                    self.state.profiles.len(), players_from_spec),
                "missing_hint": if has_players { "" } else {
                    "尚无参与者。请用 `ask_user` 收集对手名单，或用 `web_search` 取证后 `participant_profile_update` 登记。"
                },
            },
            "added_value": {
                "present": has_added_value,
                "evidence": if has_added_value { "已有参数/策略结构" } else { "无" },
                "missing_hint": if has_added_value { "" } else {
                    "尚无可比较的支付结构。请先建模（`gamespec_build`）——附加值只能在支付上算出来。"
                },
            },
            "rules": {
                "present": has_rules,
                "evidence": format!("带来源的行为事件 {} 条",
                    self.state.profiles.iter().map(|p| p.timeline.len()).sum::<usize>()),
                "missing_hint": if has_rules { "" } else {
                    "尚无规则依据。定价/渠道/平台规则会影响可行策略集，缺了会把不可行的行动算成可行。"
                },
            },
            "tactics": {
                "present": has_tactics,
                "evidence": if has_tactics { "已建模" } else { "未建模" },
                "missing_hint": if has_tactics { "" } else {
                    "尚未建立博弈模型，战术层无法评估。"
                },
            },
            "scope": {
                "present": has_scope,
                "evidence": if has_scope { "剧本已声明扫描范围" } else { "剧本未声明" },
                "missing_hint": if has_scope { "" } else {
                    "剧本未声明价值网扫描范围——边界外可能还有参与者。请在剧本里补 `value_net`。"
                },
            },
        });

        let present_count = [has_players, has_added_value, has_rules, has_tactics, has_scope]
            .iter()
            .filter(|x| **x)
            .count();

        ExecOutcome::Ok(
            serde_json::json!({
                "parts": parts,
                "present": present_count,
                "total": 5,
                "complete": present_count == 5,
                "note": "这是**材料完备性**检查（够不够建模），不是模型选择。\
                         缺口必须如实标注：缺项会让建模漏掉矩阵外的真实约束，\
                         而那种漏不会让结果「看起来错」，只会让它悄悄不对。",
            })
            .to_string(),
        )
    }

    /// `case_store_write`：把本次咨询归档进案例库（§7.2 的闭环起点）。
    ///
    /// # 为什么归档是**回测闭环**的关键而非"顺手存一份"
    ///
    /// §12 的风险表把"案例库冷启动无回测"列为风险，对策是**积累样本后校准
    /// 判据权重**。而校准需要 `(预测, 结果)` 成对——预测在**现在就固定**
    /// （`distribution` + 权重快照），结果事后回填。若归档时不记预测，
    /// 三个月后就有了结果却无从比对：那份数据对校准**毫无用处**。
    ///
    /// 故本工具强制要求"此刻已有预测"，缺则拒绝归档。
    fn case_store_write(&mut self, args: &serde_json::Value) -> ExecOutcome {
        // ── 归档必须带上"此刻的预测" ──
        //
        // ⚠️ 但"预测"的形态**按模式而异**（这是第三次遇到同一个形态了）：
        //
        // | 模式 | 此刻的预测 |
        // |---|---|
        // | scenario | 均衡概率分布（`select_fuse`） |
        // | whatif | 条件后果报告（各对手反应的概率） |
        // | discover | 满意策略集（各候选的达标概率） |
        //
        // 早先只认分布，于是 **whatif/discover 模式的 `archived` 不可达**——
        // 而 archived 是终态，这意味着那两个模式**永远收不了尾**。
        //
        // 三者的共同点仍然是：**它们是引擎算出的、可日后比对的量**。
        // 那才是"归档要带预测"的本意（没有它，事后的结果无从比对，
        // 那份记录对校准毫无用处）。
        // 「此刻的预测」按模式取值。三者都满足 `CasePrediction` 的语义
        // （**候选 + 概率**），只是候选的含义不同：
        //
        // | 模式 | 候选 | 概率 |
        // |---|---|---|
        // | scenario | 均衡 id | 均衡概率 |
        // | whatif | 对手反应 | 该反应的概率 |
        // | discover | 达标候选 | 最低达标概率（作为该候选的把握度） |
        //
        // ⚠️ 这不是"把不相关的数塞进同一个结构"：**回测问的是同一件事**——
        // "当时预测了什么、后来发生了什么"。whatif 的问法是"我预测对手会
        // 以概率 p 降价"，discover 的问法是"我预测候选 c 达标的把握是 p"。
        // 三者都能在事后回填事实并比对。
        let (candidates, probabilities, weights_snapshot, pred_kind): (
            Vec<String>,
            Vec<f64>,
            BTreeMap<String, f64>,
            shangyi_kb::PredictionKind,
        ) = match self.recommendation_basis() {
            Some(RecommendationBasis::EquilibriumDistribution) => {
                let dist = self
                    .state
                    .distribution
                    .as_ref()
                    .expect("basis checked");
                (
                    dist.entries.iter().map(|e| e.equilibrium_id.clone()).collect(),
                    dist.entries.iter().map(|e| e.prob).collect(),
                    // 权重快照：未校准时给等权（**如实如此**，不假装已校准）
                    dist.entries
                        .iter()
                        .flat_map(|e| e.drivers.iter())
                        .collect::<std::collections::BTreeSet<_>>()
                        .into_iter()
                        .map(|s| (s.as_str().to_string(), 1.0))
                        .collect(),
                    shangyi_kb::PredictionKind::EquilibriumDistribution,
                )
            }
            Some(RecommendationBasis::ConsequenceReport) => {
                let cr = self
                    .state
                    .consequence_report
                    .as_ref()
                    .expect("basis checked");
                (
                    cr.reactions.iter().map(|r| r.reaction.clone()).collect(),
                    cr.reactions.iter().map(|r| r.probability).collect(),
                    // whatif 没有判据权重（没走 select），如实给空表
                    BTreeMap::new(),
                    // 对手必选其一 ⇒ 反应概率是**归一化**的分布
                    shangyi_kb::PredictionKind::ConditionalReactions,
                )
            }
            Some(RecommendationBasis::DiscoveryReport) => {
                let dr = self
                    .state
                    .discovery_report
                    .as_ref()
                    .expect("basis checked");
                (
                    dr.satisfied
                        .iter()
                        .map(|s| s.candidate.id.clone())
                        .collect(),
                    dr.satisfied
                        .iter()
                        .map(|s| {
                            s.criteria
                                .iter()
                                .map(|c| c.achieved_probability)
                                .fold(f64::INFINITY, f64::min)
                        })
                        .collect(),
                    BTreeMap::new(),
                    // 达标概率**不归一化**（多个候选可同时达标）——见该变体的说明
                    shangyi_kb::PredictionKind::SatisficingAchievement,
                )
            }
            None => {
                return ExecOutcome::Failed(
                    "尚无「此刻的预测」可归档——需要下列之一（取决于模式）：\n\
                     · scenario：`select_fuse` 的均衡概率分布\n\
                     · whatif：`consequence_predict` 的条件后果报告\n\
                     · discover：`discover_satisfice` 的满意策略评估\n\
                     归档必须带上预测——否则事后有结果也无从比对，\
                     那份记录对校准毫无用处。"
                        .into(),
                );
            }
        };

        let case_id = args
            .get("case_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed-case")
            .to_string();
        // ⚠️ **这条重复检查只对"归档新案例"生效，不拦"回填结果"**。
        //
        // 回填**本来就是**针对已存在的案例（先归档冻结预测，事后回填真相）。
        // 把两者用同一个检查拦住，会让回填永远不可达——
        // 而那是校准闭环的第五步（实测：我刚实现回填时就被它拦住了）。
        //
        // 判据是"本次调用是否带 `outcome`"：
        // - 不带 ⇒ 是归档 ⇒ 重复必须拒（覆盖会丢失原有预测留档）
        // - 带 ⇒ 是回填 ⇒ 案例必须已存在（由回填分支自己校验）
        let is_backfill = args.get("outcome").is_some();
        if !is_backfill && self.state.case_library.get(&case_id).is_some() {
            return ExecOutcome::Failed(format!(
                "案例 `{case_id}` 已存在。入库不得覆盖——覆盖会丢失原有的预测留档。"
            ));
        }

        let date = args
            .get("date")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let prediction = shangyi_kb::CasePrediction {
            candidates,
            distribution: probabilities,
            selector_weights_snapshot: weights_snapshot,
            kind: pred_kind,
        };

        // 假设清单来自画像（cp2 的硬假设在 GameSpec 的 params 里）
        let assumptions: Vec<shangyi_kb::CaseAssumption> = self
            .state
            .game_spec
            .as_ref()
            .map(|g| {
                g.params
                    .iter()
                    .map(|(k, p)| shangyi_kb::CaseAssumption {
                        id: k.clone(),
                        content: p.name.clone(),
                        source: format!("{:?}", p.source),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut rec = match shangyi_kb::CaseRecord::new(
            case_id.clone(),
            date,
            self.playbook.meta.id.clone(),
            prediction,
        ) {
            Ok(r) => r,
            Err(e) => return ExecOutcome::Failed(format!("案例构造失败：{e}")),
        };
        // ── 来源（`provenance`）——校准的第一道闸门 ──
        //
        // **必填且不设默认**：默认值在这里是有害的。
        // 若默认 `real_engagement`，演练案例会被静默当成真实委托；
        // 若默认 `exercise`，真实咨询会被静默排除、校准永远攒不够。
        // 两个方向都造成"看不出来"的错，故要求调用方明说。
        //
        // 这条与铁律 2 同源：**没有来源的东西不得进入结论**。
        // 一个案例的来源没说清，它对校准就是不可用的。
        // 回填时**沿用原有记录的来源**——不让调用方重报一次。
        // 重报没有意义（来源是归档时的属性），且会给"悄悄改来源"开口子：
        // 演练案例回填时声称自己是真实委托，就能混进校准集。
        if is_backfill {
            if let Some(prev) = self.state.case_library.get(&case_id) {
                rec.provenance = prev.provenance;
            }
        } else {
        rec.provenance = match args.get("provenance").and_then(|v| v.as_str()) {
            Some("real_engagement") => shangyi_kb::CaseProvenance::RealEngagement,
            Some("exercise") => shangyi_kb::CaseProvenance::Exercise,
            Some(other) => {
                return ExecOutcome::Failed(format!(
                    "`provenance` 取值非法：`{other}`。\n                     合法值只有两个：\n                     · `real_engagement` —— **真实客户委托**（事后有真实结果可回填）\n                     · `exercise` —— 演练/样例/批量跑批（**不会有**真实结果）"
                ))
            }
            None => {
                return ExecOutcome::Failed(
                    "`case_store_write` 需要 `provenance`（案例来源）——它决定这条记录\
                     日后能否参与判据权重校准。\n                     · `real_engagement` —— 真实客户委托\n                     · `exercise` —— 演练/样例/批量跑批\n                     \n                     为什么必须明说：**演练案例的「结果」是编出来的**，\
                     拿它调权重会产出「用假数据拟合的可信度」。\
                     故只有 `real_engagement` 会被校准采纳；\
                     而把真实委托误标成演练，会让校准永远攒不够样本。\
                     两个方向都看不出来，所以不允许默认。"
                        .into(),
                )
            }
        };
        }

        rec.client_anonymous = args
            .get("client")
            .and_then(|v| v.as_str())
            .unwrap_or("匿名客户")
            .to_string();
        rec.decision_question = args
            .get("question")
            .and_then(|v| v.as_str())
            .unwrap_or("（未记录决策问题）")
            .to_string();
        rec.assumptions = assumptions;

        // ── 回填结果（可选）：`outcome` 键 ──
        //
        // 校准闭环的第五步（见 `CaseLibrary::backtest_records` 的文档）：
        // 归档冻结预测 → **事后回填结果** → 转记录 → calibrate → 新权重。
        //
        // 没有这一步，案例库里的样本**永远停留在"待回填"**，
        // 而 `calibratable` 恒为 0 ⇒ 判据权重永远未校准。
        if let Some(o) = args.get("outcome") {
            let outcome: shangyi_kb::CaseOutcome = match serde_json::from_value(o.clone()) {
                Ok(x) => x,
                Err(e) => {
                    return ExecOutcome::Failed(format!(
                        "`outcome` 解析失败：{e}。\n\
                         形状：{{\"date\": \"2026-06-01\", \"what_happened\": \"对手跟进降价\", \
                         \"equilibrium_realized\": \"e1\"}}\n\
                         ⚠️ `equilibrium_realized` 必须是**预测候选之一**——\
                         若实际发生的情况不在候选里，那说明建模漏掉了它（独立信号，\
                         不是硬塞进对数损失的理由）。"
                    ))
                }
            };
            let Some(existing) = self.state.case_library.get_mut(&case_id) else {
                return ExecOutcome::Failed(format!(
                    "案例 `{case_id}` 不在库里——回填结果前必须先归档（冻结预测）。"
                ));
            };
            if let Err(e) = existing.resolve(outcome) {
                return ExecOutcome::Failed(format!("回填失败：{e}"));
            }
            // 落盘（若有路径）
            let saved = self.persist_case_library();
            return match saved {
                Ok(()) => ExecOutcome::Ok(
                    serde_json::json!({
                        "case_id": case_id,
                        "outcome_backfilled": true,
                        "calibratable": self.state.case_library.calibratable().len(),
                        "note": "结果已回填。预测与结果**成对**的样本才可用于校准——\
                                 这一条已进入可校准集合。",
                    })
                    .to_string(),
                ),
                Err(e) => ExecOutcome::Failed(format!(
                    "结果已回填到内存，但**存盘失败**：{e}\n\
                     ⚠️ 这很危险：进程退出后这条样本会丢失。请检查磁盘后重试。"
                )),
            };
        }

        match self.state.case_library.insert(rec) {
            Ok(()) => {
                // ⚠️ **必须落盘**——否则进程退出后案例丢失，案例库永远
                // 积累不到校准所需的样本（见 `case_library_path` 的文档）。
                if let Err(e) = self.persist_case_library() {
                    return ExecOutcome::Failed(format!(
                        "案例已入库（内存），但**存盘失败**：{e}\n\
                         ⚠️ 请立刻检查磁盘——案例库是几个月积累的样本，无法重建。"
                    ));
                }
                ExecOutcome::Ok(
                    serde_json::json!({
                        "case_id": case_id,
                        "library_size": self.state.case_library.len(),
                        "prediction_frozen": true,
                        "persisted": self.state.case_library_path.is_some(),
                        "note": "预测已固定。事后请用 `case_store_write` 的 `outcome` 回填实际结果——\
                                 只有预测与结果成对，样本才能用于校准判据权重。",
                    })
                    .to_string(),
                )
            }
            Err(e) => ExecOutcome::Failed(format!("入库失败：{e}")),
        }
    }

    /// 把案例库落盘（未配置路径时是**空操作**，并如实返回说明）。
    fn persist_case_library(&self) -> Result<(), String> {
        let Some(path) = &self.state.case_library_path else {
            // 未配置路径 ⇒ 不落盘。**不报错**（那是合法的只读/测试场景），
            // 但要如实说明——静默成功会让调用方以为已经存了。
            return Ok(());
        };
        self.state
            .case_library
            .save(path)
            .map_err(|e| format!("{e}"))
    }

    /// `case_store_read`：读案例库（校准与冷启动判断的数据源）。
    fn case_store_read(&self, args: &serde_json::Value) -> ExecOutcome {
        let lib = &self.state.case_library;
        let total = lib.len();
        let calibratable = lib.calibratable().len();
        let pending = lib.pending().len();
        let area = lib.mean_log_loss();

        // 可选：按 case_id 取单条
        if let Some(id) = args.get("case_id").and_then(|v| v.as_str()) {
            return match lib.get(id) {
                Some(c) => match serde_json::to_string_pretty(c) {
                    Ok(j) => ExecOutcome::Ok(j),
                    Err(e) => ExecOutcome::Failed(format!("序列化失败：{e}")),
                },
                None => ExecOutcome::Failed(format!(
                    "案例库中没有 `{id}`。库中现有 {} 条：{}",
                    total,
                    lib.ids().into_iter().collect::<Vec<_>>().join("、")
                )),
            };
        }

        ExecOutcome::Ok(
            serde_json::json!({
                "total": total,
                "calibratable": calibratable,
                "pending_outcome": pending,
                "mean_log_loss": area,
                "calibration_ready": lib.calibration_ready(shangyi_selection::MIN_SAMPLES_FOR_CALIBRATION),
                "min_samples_needed": shangyi_selection::MIN_SAMPLES_FOR_CALIBRATION,
                "note": if total == 0 {
                    "案例库为空。**空库不等于「没有可用先验」**——它表示判据权重尚未校准，\
                     报告必须如实标注这一点，而不得让读者以为概率已校准。"
                } else if calibratable < shangyi_selection::MIN_SAMPLES_FOR_CALIBRATION {
                    "样本不足，判据权重仍为未校准先验。"
                } else {
                    "样本已够，可运行回测校准判据权重。"
                },
            })
            .to_string(),
        )
    }

    /// `report_render`：把会话产物渲染为 [`shangyi_report::Report`]。
    ///
    /// # 铁律 1 在报告层的落点
    ///
    /// 报告里的**每一个**数字都必须带 provenance（`ProvenanceId`），
    /// 缺一个则 `Report::check_provenance` 失败、报告不得交付。
    /// 这不是格式要求：它使"这个数字哪来的"在报告里可被逐一追溯，
    /// 从而让"模型编了一个数"这类事故在**交付前**暴露，而不是在客户质疑时。
    ///
    /// # 它刻意不做的事
    ///
    /// **不渲染成交付格式**（PDF/docx）。渲染是 `Sidecar` 的职责（§15.1：
    /// 规格/渲染分离，数字不得由渲染侧产生）。本工具产出
    /// **结构化报告模型**，渲染只是它的投影。
    fn report_render(&mut self, args: &serde_json::Value) -> ExecOutcome {
        use shangyi_report::{
            Number, ProvenanceId, ProvenanceKind, ProvenanceRecord, Report, Section,
        };

        let mut report = Report::new(args.get("case_id").and_then(|v| v.as_str()).unwrap_or("case"));

        // ── ① 老板页：最可能的均衡 + 置信度 ──
        let mut boss = Section::new("boss_page", "一页结论");
        let mut boss_numbers: Vec<String> = Vec::new();

        match self.recommendation_basis() {
            Some(RecommendationBasis::EquilibriumDistribution) => {
                let dist = self
                    .state
                    .distribution
                    .as_ref()
                    .expect("basis checked");
                if let Some((top, ties)) = dist.most_likely() {
                    let pid = ProvenanceId("prov_dist".into());
                    // 必须注册到 **report 自己的台账**（`Report::add_number` 查的是它，
                    // 不是任何局部 ledger——早先的写法正因此报"数字缺少 provenance"）
                    if let Err(e) = report.register_provenance(
                        ProvenanceRecord::new("prov_dist", ProvenanceKind::CodeComputed, 0)
                            // 溯源到**哪个引擎**算的，而不只是"某个工具"
                            .with_code("shangyi-selection", "0.1.0", "fuse"),
                    ) {
                        return ExecOutcome::Failed(format!("provenance 注册失败：{e}"));
                    }
                    let key = "prob_most_likely";
                    if let Err(e) = report.add_number(
                        Number::new(
                            key,
                            top.prob,
                            format!("均衡「{}」的概率", top.equilibrium_id),
                            pid,
                        )
                        .with_unit("概率"),
                    ) {
                        return ExecOutcome::Failed(format!("数字登记失败：{e}"));
                    }
                    boss_numbers.push(key.to_string());
                    let tie_note = if ties {
                        "（存在并列——最高概率不唯一，请勿只报一个）"
                    } else {
                        ""
                    };
                    boss = boss.with_paragraph(format!(
                        "最可能的均衡是「{}」，概率 {:.1}%{}。",
                        top.equilibrium_id,
                        top.prob * 100.0,
                        tie_note
                    ));
                    if !top.drivers.is_empty() {
                        boss = boss.with_paragraph(format!(
                            "主要判据：{}。",
                            top.drivers
                                .iter()
                                .map(|d| d.as_str())
                                .collect::<Vec<_>>()
                                .join("、")
                        ));
                    }
                }
                // 置信度必须如实进老板页（不得藏在附录）
                boss = boss.with_paragraph(format!("置信度：{}", dist.confidence.note()));
                if !dist.divergences.is_empty() {
                    boss = boss.with_paragraph(format!(
                        "⚠ 判据存在分歧（{} 条）：{}",
                        dist.divergences.len(),
                        dist.divergences.join("；")
                    ));
                }
            }
            Some(RecommendationBasis::ConsequenceReport) => {
                let cr = self
                    .state
                    .consequence_report
                    .as_ref()
                    .expect("basis checked");
                if let Err(e) = report.register_provenance(
                    ProvenanceRecord::new("prov_consequence", ProvenanceKind::CodeComputed, 0)
                        .with_code("shangyi-consequence", "0.1.0", "residual_subgame"),
                ) {
                    return ExecOutcome::Failed(format!("provenance 注册失败：{e}"));
                }
                boss = boss.with_paragraph(format!(
                    "条件后果推演：我方动作「{}」，共 {} 种对手反应。",
                    cr.decision,
                    cr.reactions.len()
                ));
                // 逐个反应登记反应概率（它们是引擎算出的数字）
                for (i, r) in cr.reactions.iter().enumerate() {
                    let key = format!("reaction_prob_{i}");
                    if let Err(e) = report.add_number(Number::new(
                        &key,
                        r.probability,
                        format!("对手反应「{}」的概率", r.reaction),
                        ProvenanceId("prov_consequence".into()),
                    )) {
                        return ExecOutcome::Failed(format!("数字登记失败：{e}"));
                    }
                    boss_numbers.push(key);
                    boss = boss.with_paragraph(format!(
                        "· {}（概率 {:.2}）：{}",
                        r.reaction, r.probability, r.drivers.join("；")
                    ));
                }
                boss = boss.with_paragraph(format!(
                    "注意：这是**条件推演**（我方固定在「{}」），不是「最可能发生什么」的概率预测。",
                    cr.decision
                ));
            }
            Some(RecommendationBasis::DiscoveryReport) => {
                let dr = self
                    .state
                    .discovery_report
                    .as_ref()
                    .expect("basis checked");
                if let Err(e) = report.register_provenance(
                    ProvenanceRecord::new("prov_discover", ProvenanceKind::CodeComputed, 0)
                        .with_code("shangyi-discover", "0.1.0", "satisfice"),
                ) {
                    return ExecOutcome::Failed(format!("provenance 注册失败：{e}"));
                }
                boss = boss.with_paragraph(format!(
                    "满意策略发现：{} 个候选达标，{} 条放宽建议。",
                    dr.satisfied.len(),
                    dr.relaxation.len()
                ));
                for (i, s) in dr.satisfied.iter().enumerate() {
                    let key = format!("satisfied_achv_{i}");
                    // 达标概率是引擎算出的核心数字
                    // 该候选在各准则上的**最低**达标概率——它是"是否满意"的依据
                    let min_achv = s
                        .criteria
                        .iter()
                        .map(|c| c.achieved_probability)
                        .fold(f64::INFINITY, f64::min);
                    if let Err(e) = report.add_number(Number::new(
                        &key,
                        min_achv,
                        format!("候选「{}」的最低达标概率", s.candidate.id),
                        ProvenanceId("prov_discover".into()),
                    )) {
                        return ExecOutcome::Failed(format!("数字登记失败：{e}"));
                    }
                    boss_numbers.push(key);
                    boss = boss.with_paragraph(format!(
                        "· {}：最低达标概率 {:.2}",
                        s.candidate.id, min_achv
                    ));
                }
            }
            None => {
                boss = boss.with_paragraph("本会话尚无任何引擎产出的结论（分布/后果报告/满意评估都缺）。");
            }
        }
        boss = boss.with_numbers(boss_numbers.clone());
        report.add_section(boss);

        // ── ② 情景层：解集与选择依据 ──
        let mut scen = Section::new("scenario", "情景推演");
        if let Some(eq) = &self.state.equilibria_json {
            if let Err(e) = report.register_provenance(
                ProvenanceRecord::new("prov_solve", ProvenanceKind::CodeComputed, 0)
                    .with_code("shangyi-solver", "0.1.0", "support_enumeration"),
            ) {
                return ExecOutcome::Failed(format!("provenance 注册失败：{e}"));
            }
            scen = scen.with_paragraph(format!(
                "均衡解集（确定性求解器输出，未做任何人工调整）：
{}",
                truncate_str(eq, 1200)
            ));
        } else {
            scen = scen.with_paragraph("尚未求解。");
        }
        report.add_section(scen);

        // ── ③ 画像与证据层 ──
        let mut prof = Section::new("profile_evidence", "参与者画像与证据");
        if self.state.profiles.is_empty() {
            prof = prof.with_paragraph("尚无参与者画像。");
        } else {
            for p in &self.state.profiles {
                let sourced = p.timeline.len();
                prof = prof.with_paragraph(format!(
                    "{}（{}）：{} 条带来源的行为事件；类型后验 {}。",
                    p.participant_id,
                    shangyi_consequence::role_label(p.value_net_role),
                    sourced,
                    p.most_likely_type()
                        .map(|(t, pr)| format!("{t} {:.0}%", pr * 100.0))
                        .unwrap_or_else(|| "未推断".into())
                ));
            }
        }
        report.add_section(prof);

        // ── ④ 假设清单（铁律 2 的落点）──
        let mut assump = Section::new("assumptions", "关键假设清单（已由用户确认）");
        if let Some(g) = &self.state.game_spec {
            if g.params.is_empty() {
                assump = assump.with_paragraph("本模型无显式参数。");
            }
            for (k, p) in &g.params {
                assump = assump.with_paragraph(format!(
                    "`{k}` = {}（来源：{:?}）",
                    p.name, p.source
                ));
            }
        } else {
            assump = assump.with_paragraph("尚未建模，无假设清单。");
        }
        report.add_section(assump);

        // ── 交付前自检（铁律 1 的落点）──
        if let Err(e) = report.check_provenance() {
            // 空报告（一个数字都没有）也被自检拒绝，且这是**正确**的：
            // 一次还没算出任何结果的咨询，不该产出"看起来像报告"的文档——
            // 它只有散文没有数，读者却会以为分析做完了。
            //
            // ⚠️ 诊断必须**直接判断"有没有数字"**，不能拿别的状态去推断。
            //
            // 早先这里写的是 `if self.state.distribution.is_none()`——
            // 即用"没有分布"**推断**"没有数字"。那在 scenario 模式下碰巧成立
            // （分布是数字的主要来源），但在 **whatif / discover 模式下是错的**：
            // 那两条路径**没有 select 阶段**，永远不会有分布，而它们的
            // "数字"来自后果报告（对手反应概率、各方支付）与满意策略评估。
            //
            // 于是那两条路径的 `report` 阶段**结构性不可达**——
            // 每次都被判为"未完成 select 阶段"，而它们根本不走 select。
            // （实测：脚本化 E2E 在 whatif/discover 上同时卡在这里。）
            //
            // 修法：**类型化匹配**错误变体——`check_provenance` 自己就知道
            // 失败原因（`EmptyReport` 与其他变体是不同的东西），
            // 而我们此前用一个不相干的状态去推断它。
            let n_numbers = report.numbers.len();
            if matches!(
                e,
                shangyi_report::ProvenanceError::EmptyReport
            ) {
                return ExecOutcome::Failed(
                    "本次咨询**尚未算出任何可报告的数字**，故不产出报告。\
                     只含散文的报告会让读者以为分析已完成——那比不出报告更坏。\n\
                     → 各模式的数字来源：scenario 走 `solve_equilibria` + `select_fuse`；\
                       whatif 走 `consequence_predict`；discover 走 `discover_satisfice`。"
                        .into(),
                );
            }
            return ExecOutcome::Failed(format!(
                "报告自检未通过（{}）。报告**不得交付**——缺 provenance 的数字无法回答\
                 「这个数哪来的」，而那是铁律 1 的核心要求。\
                 （本次报告含 {n_numbers} 个数字。）",
                e
            ));
        }

        self.state.report_model = Some(report.clone());
        // ⚠️ 必须同时写 `report_json`：`artifacts()` 用它登记 `ReportBundle`
        // 产物，而阶段图要求 report 阶段产出它才能进 archived。
        //
        // 缺陷背景（实测暴露）：早先只写 `report_model`，于是
        // `report_render` **成功但产物登记不上** ⇒ archived 不可达。
        // 与 `recommend_build` 未注册是同一类问题的两种形态——
        // **工具成功了，但阶段要求的那份产物没被登记**。
        self.state.report_json = Some(serde_json::to_string(&report).unwrap_or_default());
        match serde_json::to_string_pretty(&report) {
            Ok(j) => ExecOutcome::Ok(
                serde_json::json!({
                    "report": serde_json::from_str::<serde_json::Value>(&j).unwrap_or_default(),
                    "sections": report.sections.len(),
                    "provenance_records": report.ledger_len(),
                    "note": "这是结构化的**报告模型**，不是渲染产物。\
                             渲染（PDF/docx）由侧车完成——数字只能出自本层（§15.1）。",
                })
                .to_string(),
            ),
            Err(e) => ExecOutcome::Failed(format!("报告序列化失败：{e}")),
        }
    }

    /// `web_search` / `web_fetch`：外部检索。
    ///
    /// 未绑定 [`SearchProvider`] ⇒ `Blocked`（**不是** `Failed`）：
    /// 语义是"无法评估"，与"抓了但没有结果"必须可区分（§7.1、不变量②）。
    fn retrieval_budget(&self) -> Result<(), ExecOutcome> {
        let count=self.state.retrieval_attempts.get();
        if count>=16 { return Err(ExecOutcome::Blocked("本次咨询已用完 16 次检索/抓取预算。停止调用网络工具，整理已取得证据；未核实或缺失内容保留未知，不能把未取得解释为不存在，也不能用推测补全。".into())); }
        self.state.retrieval_attempts.set(count+1); Ok(())
    }

    /// `poi_survey`：按地址查周边门店计数。
    ///
    /// # 为什么它也算检索预算
    ///
    /// 这是真实网络调用（含限流的付费接口），与 `web_search` 同属"外部取证"。
    /// 不计数就会让预算形同虚设：模型可以无限次换地址刷计数。
    fn poi_survey(&mut self, call: &crate::agent::ToolCall, args: &serde_json::Value) -> ExecOutcome {
        let address = args.get("address").and_then(|v| v.as_str()).unwrap_or("").trim();
        let radius = args.get("radius").and_then(|v| v.as_u64()).unwrap_or(500);
        let poi_label = format!(
            "poi:{}@{}m",
            if address.is_empty() { "unset" } else { address },
            radius
        );
        // 先看有没有数据源：这是配置事实，与参数对不对无关。没有它就只剩「未取得」，
        // 报「缺 address」会把配置问题说成参数问题（该去申请 key 的人会去改参数）。
        let Some(provider) = self.poi else {
            if let Some(task) = self.state.research_task.as_mut() {
                task.observe_poi(
                    &poi_label,
                    false,
                    Some("未绑定周边数据源（PoiProvider）"),
                    self.state.retrieval_attempts.get(),
                    16,
                );
            }
            return ExecOutcome::Blocked(
                "未绑定周边数据源（PoiProvider）。周边门店计数**未取得**——\
                 不得解释为「周边没有门店」，也不得用推测填补。\
                 配置方式：设 AMAP_KEY（或写入 ~/.shangyi-secrets/amap.env）后重启。"
                    .into(),
            );
        };
        if address.is_empty() {
            return ExecOutcome::Failed("`poi_survey` 需要 `address`（要查的地址）。".into());
        }
        let count_before = self.state.retrieval_attempts.get();
        if let Err(out) = self.retrieval_budget() {
            return out;
        }
        let query = PoiQuery {
            address: address.to_string(),
            radius: radius.min(u32::MAX as u64) as u32,
            brand: args.get("brand").and_then(|v| v.as_str()).unwrap_or("").trim().to_string(),
            keywords: args.get("keywords").and_then(|v| v.as_str()).unwrap_or("").trim().to_string(),
        };
        match provider.survey(&query) {
            Ok(survey) => {
                // 登记回执：值、来源、口径都在这里定死，模型只能引用编号。
                // 这是"结构化证据不可伪造"的落点——回执由工具产生。
                let mut receipts = Vec::new();
                let scope = format!("{} · {} 米", survey.address, survey.radius);
                if let Some(task) = self.state.research_task.as_mut() {
                    task.observe_poi(&scope, true, None, self.state.retrieval_attempts.get(), 16);
                }
                let register = |label: &str, count: u64, receipts: &mut Vec<serde_json::Value>, sink: &mut Vec<(String, String, u64, String)>| {
                    let id = format!("{}#{}", call.call_id, receipts.len() + 1);
                    sink.push((id.clone(), label.to_string(), count, scope.clone()));
                    receipts.push(serde_json::json!({
                        "receipt": id, "label": label, "value": format!("{count} 家"),
                    }));
                };
                let mut pending: Vec<(String, String, u64, String)> = Vec::new();
                for item in &survey.counts {
                    register(&item.label, item.count, &mut receipts, &mut pending);
                }
                if let Some(brand) = &survey.brand {
                    register(&brand.label, brand.count, &mut receipts, &mut pending);
                }
                if let Some(keywords) = &survey.keywords {
                    register(&keywords.label, keywords.count, &mut receipts, &mut pending);
                }
                for (id, _label, count, scope) in pending {
                    self.state.structured_receipts.insert(
                        id.clone(),
                        crate::research_task::StructuredEvidence {
                            receipt: id,
                            kind: "高德 POI 周边计数".into(),
                            scope: scope.clone(),
                            value: format!("{count} 家"),
                            source: survey.source.clone(),
                        },
                    );
                }
                ExecOutcome::Ok(
                serde_json::json!({
                    "address": survey.address,
                    "district": survey.district,
                    "radius": survey.radius,
                    "counts": survey.counts.iter().map(|c| serde_json::json!({
                        "label": c.label, "count": c.count,
                    })).collect::<Vec<_>>(),
                    "brand": survey.brand.as_ref().map(|b| serde_json::json!({
                        "label": b.label, "count": b.count,
                    })),
                    "keywords": survey.keywords.as_ref().map(|k| serde_json::json!({
                        "label": k.label, "count": k.count,
                    })),
                    "unavailable": survey.unavailable,
                    "source": survey.source,
                    // 回执编号：把这些计数登记进请求字段时逐字引用，
                    // 不要自己复述数值（复述的一律拒收）。
                    "receipts": receipts,
                    "receipt_usage": "把某项计数登记为请求字段依据时，用 research_task_update 的 structured 传对应 receipt 编号；数值由回执给出，不要手写。",
                    // 来源可追溯到具体地址与半径：计数不是"某篇文章说的"，
                    // 而是"某坐标某半径内的 POI 计数"，两者证据等级不同。
                    "provenance": {
                        "kind": "public_data",
                        "source": survey.source,
                        "address": survey.address,
                        "radius": survey.radius,
                    },
                    "note": "计数是「该半径内该类目的门店数量」，不是营业额或客流。\
                             未列出的类目为未取得，不等于 0。",
                })
                .to_string(),
                )
            }
            // 未配置后端不消耗 16 次真实网络预算。
            Err(SearchUnavailable::NoBackend { detail }) => {
                self.state.retrieval_attempts.set(count_before);
                if let Some(task) = self.state.research_task.as_mut() {
                    task.observe_poi(&poi_label, false, Some(&detail), count_before, 16);
                }
                ExecOutcome::Blocked(format!(
                    "周边数据源未配置：{detail}。周边计数**未取得**，不等于没有门店。"
                ))
            }
            Err(SearchUnavailable::Failed { detail, .. }) => {
                if let Some(task) = self.state.research_task.as_mut() {
                    task.observe_poi(
                        &poi_label,
                        false,
                        Some(&detail),
                        self.state.retrieval_attempts.get(),
                        16,
                    );
                }
                ExecOutcome::Failed(format!("周边查询失败：{detail}"))
            }
        }
    }

    fn web_search(&mut self, args: &serde_json::Value) -> ExecOutcome {
        let query = args.get("query").and_then(|v|v.as_str()).unwrap_or_default();
        if query.trim().is_empty() {
            return ExecOutcome::Failed("`web_search` 需要 `query`。".into());
        }
        let Some(sp) = self.search else {
            if let Some(task)=self.state.research_task.as_mut() {
                task.observe_search_failure(query,"未绑定检索后端",self.state.retrieval_attempts.get(),16);
            }
            return ExecOutcome::Blocked(
                "未绑定检索后端（SearchProvider）。公开数据检索需要真实后端；\
                 在它就位前，本项情报**未取得**——不得用推测填补。"
                    .into(),
            );
        };
        let count_before = self.state.retrieval_attempts.get();
        if let Err(out)=self.retrieval_budget() {return out;}
        match sp.search_with_health(query) {
            Ok((hits, health)) => {
                let used = self.state.retrieval_attempts.get();
                if let Some(task) = self.state.research_task.as_mut() {
                    task.observe_search(query, &hits.iter().map(|hit| (hit.title.clone(), hit.url.clone())).collect::<Vec<_>>(), &health, used, 16);
                }
                let conclusive_empty = hits.is_empty() && health.empty_result_is_conclusive();
                ExecOutcome::Ok(serde_json::json!({
                    "query": query,
                    "hits": hits.iter().map(|h| serde_json::json!({
                        "title": h.title, "url": h.url, "snippet": h.snippet,
                        "backend": h.backend,
                        // 排序分与信号：这是**检索排序**依据，不是事实可信度。
                        // 公开出来让模型能看见"为什么这条在前"，也让审计可复算。
                        "rank_score": h.rank_score,
                        "rank_signals": h.rank_signals,
                        // 供应商日期元数据未经原文核对，字段名自带约束。
                        "provider_date_unverified": h.published_at,
                    })).collect::<Vec<_>>(),
                    "health": health,
                    "budget": {"used":used,"limit":16,"remaining":16usize.saturating_sub(used)},
                    "empty_is_a_conclusion": conclusive_empty,
                    "provenance": {"kind": "web", "query": query},
                    "note": if conclusive_empty {
                        "本次检索来源正常响应，当前查询未命中；不等于相关信息不存在。"
                    } else if hits.is_empty() {
                        "本次未取得结果，检索来源健康情况不足或有来源未响应；只能说覆盖不足，不能当作查无资料。"
                    } else {
                        "结果按检索相关性排序（rank_score 越高越靠前，可含官方域名/年份相符等信号）；\
                         这不是事实可信度，关键日期、金额与政策条件仍须用 web_fetch 读原文核对。"
                    },
                }).to_string())
            },
            // 未配置后端不消耗 16 次真实网络预算（避免把 web_fetch 额度挤占掉）。
            Err(SearchUnavailable::NoBackend { detail }) => {
                self.state.retrieval_attempts.set(count_before);
                if let Some(task)=self.state.research_task.as_mut() {
                    task.observe_search_failure(query,&detail,self.state.retrieval_attempts.get(),16);
                }
                ExecOutcome::Blocked(format!("检索后端未配置：{detail}"))
            },
            Err(e @ SearchUnavailable::Failed { .. }) => {
                if let Some(task)=self.state.research_task.as_mut() {
                    task.observe_search_failure(query,&e.actionable(),self.state.retrieval_attempts.get(),16);
                }
                // 与 `web_fetch` 同款：把"该不该重试"一并交给模型。
                // 检索与抓取共用同一份可重试判定（`retryable`），
                // 故两处的语义不会分叉。
                ExecOutcome::Blocked(format!("检索后端失败：{}", e.actionable()))
            }
        }
    }

    fn web_fetch(&self, args: &serde_json::Value) -> ExecOutcome {
        let url = args.get("url").and_then(|v| v.as_str()).unwrap_or_default();
        if url.trim().is_empty() {
            return ExecOutcome::Failed("`web_fetch` 需要 `url`。".into());
        }
        let refresh=match args.get("refresh") {
            None => false,
            Some(value) => match value.as_bool() {Some(value)=>value,None=>return ExecOutcome::Failed("refresh 须为布尔值".into())},
        };
        if !refresh {
            if let Some(mut cached)=self.state.fetched_pages.borrow().get(url).cloned() {
                cached["cached"]=serde_json::json!(true);
                cached["note"]=serde_json::json!("复用本次咨询先前读取的页面快照，抓取时间保持原值；本次未再次联网。如需重新核验时效，显式 refresh=true。");
                return ExecOutcome::Ok(cached.to_string());
            }
        }
        if let Some(failure) = self.state.fetch_failures.borrow().get(url) {
            let limit = if failure.retryable { 2 } else { 1 };
            if failure.attempts >= limit {
                return ExecOutcome::Blocked(format!(
                    "此 URL 已连续 {} 次未取得正文，已达本任务的{}上限；本次未再次联网。\
                     先前成功读取的快照仍保留，请换相关来源；新的用户任务可重新核验。",
                    failure.attempts, if failure.retryable { "可重试失败" } else { "不可重试失败" }
                ));
            }
        }
        if let Err(out)=self.retrieval_budget() {return out;}
        let Some(sp) = self.search else {
            return ExecOutcome::Blocked(
                "未绑定抓取后端（SearchProvider）。正文抓取需要真实后端；\
                 在它就位前，本项情报**未取得**。"
                    .into(),
            );
        };
        match sp.fetch(url) {
            Ok(page) => {
                if page.text.trim().is_empty() {
                    self.state.fetch_failures.borrow_mut().insert(url.to_owned(), FetchFailure { attempts: 1, retryable: false });
                    return ExecOutcome::Blocked(
                        "抓取有响应，但没有取得页面正文；此请求不能用作正文证据。先前读取的快照已保留，请换相关原始来源。".into()
                    );
                }
                {
                    let mut failures = self.state.fetch_failures.borrow_mut();
                    failures.remove(url);
                    failures.remove(&page.url);
                }
                self.state.fetched_sources.borrow_mut().insert(page.url.clone(), page.text.clone());
                let output=serde_json::json!({
                    "url": page.url,
                    "text": page.text,
                    "raw_bytes": page.raw_bytes,
                    "cached":false,
                    "retrieved_at_unix": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                    "provenance": {"kind": "web", "url": page.url},
                });
                let mut pages = self.state.fetched_pages.borrow_mut();
                // A refresh of a canonical page also refreshes every alias already
                // known to point to it. Otherwise an alias can replay the old body
                // after a successful refresh and silently reverse the evidence.
                // A changed redirect from an alias does not rewrite the old target.
                let refreshed_target = pages.get(url)
                    .and_then(|cached| cached.get("url"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|target| *target == url)
                    .map(str::to_owned);
                for cached in pages.values_mut() {
                    let target = cached.get("url").and_then(serde_json::Value::as_str);
                    if target == Some(page.url.as_str())
                        || refreshed_target.as_deref().is_some_and(|old| target == Some(old)) {
                        *cached = output.clone();
                    }
                }
                pages.insert(url.to_owned(), output.clone());
                pages.insert(page.url, output.clone());
                ExecOutcome::Ok(output.to_string())
            },
            Err(e) => {
                let retryable = matches!(&e, SearchUnavailable::Failed { retryable: true, .. });
                let mut failures = self.state.fetch_failures.borrow_mut();
                let failure = failures.entry(url.to_owned()).or_insert(FetchFailure { attempts: 0, retryable });
                failure.attempts += 1;
                failure.retryable = retryable;
                // ⚠️ 失败信息必须告诉模型**该不该重试**。
                //
                // # 为什么（实测：同一个 404 URL 被试了 4 次）
                //
                // run24 里模型对 `stats.gov.cn` 的同一个 URL 路径连续试了 4 次
                // （有些是 404、有些是证书过期），每次都只拿到"抓取未取得"。
                // 那条信息**没有回答"换个时间再试有用吗"**——
                // 而模型的合理反应就是再试一次。
                //
                // 但这两类失败的处置方向**完全相反**：
                //
                // | 失败 | 重试 | 正确动作 |
                // |---|---|---|
                // | 404 / 证书过期 / 人机验证 | ❌ 无用 | 换数据源 |
                // | 429 / 5xx / 超时 | ✅ 有可能 | 稍后重试 |
                //
                // 把"重试无用"说得像"暂时失败"，就是在鼓励浪费 turn。
                // 分类在**类型层面**完成（`SearchUnavailable::retryable` 由
                // `FetchError::retryable()` 给出），不在文本上匹配状态码——
                // 文本匹配会在措辞变化时**静默失效**，而失效的表现是
                // 模型又开始循环重试（离真因很远）。
                ExecOutcome::Blocked(format!("抓取未取得：{}", e.actionable()))
            }
        }
    }

    /// `participant_profile_update`：写画像（含贝叶斯类型推断）。
    fn participant_profile_update(&mut self, args: &serde_json::Value) -> ExecOutcome {
        // ⚠️ `updated_at_ms` 由**执行器**填，不接受模型给的值。
        //
        // # 为什么（实测：模型连续失败 7 次里的第一次）
        //
        // 它是必填的 `i64`，而模型**不知道当前时间**——LLM 只能猜一个
        // epoch 毫秒值，那是编造一个可验证为假的数字（违反铁律 1）。
        // 实测报错 `missing field updated_at_ms` 后模型开始试，
        // 而它试的方向（编一个数）恰恰是错的。
        //
        // 时间戳的正确来源是执行器持有的时钟，不是模型的猜测。
        // 模型给的值一律**覆盖**（不是"采用"）——否则日志里会混入
        // 看起来像时间、实际是幻觉的数字，而审计要靠它排序。
        let mut normalized = args.clone();
        if let Some(o) = normalized.as_object_mut() {
            o.remove("updated_at_ms");
        }
        let profile: ParticipantProfile = match serde_json::from_value(normalized) {
            Ok(p) => p,
            Err(e) => {
                // 同样给**针对性**提示：`role_specific` 是邻接标记，
                // 实测模型漏了 `role` 或 `details`（各 2 次）。
                let hint = if e.to_string().contains("role") || e.to_string().contains("details") {
                    "\n→ `role_specific` 的形状是**邻接标记**：\n\
                     { \"role\": \"supplier\", \"details\": { \"supply_concentration\": 0.7, … } }\n\
                     `role` 与 `details` **两者缺一即失败**（只给 details 不行）。\n\
                     可用变体：supplier / customer / substitutor / complementor\n"
                } else {
                    ""
                };
                return ExecOutcome::Failed(format!("画像解析失败：{e}{hint}"));
            }
        };
        // 画像的每条时间线事件都必须带来源——`TimelineEvent.source` 是
        // `DataSource`（非 Option），其构造器已做合规检查（禁止个人信息来源）。
        // 故这里的检查是"用户口述也要被显式标注"，而不是"可以没有来源"。
        let personal: Vec<&str> = profile
            .timeline
            .iter()
            .filter(|t| t.source.kind == SourceKind::Personal)
            .map(|t| t.date.as_str())
            .collect();
        if !personal.is_empty() {
            return ExecOutcome::Failed(format!(
                "画像含个人信息来源（{} 条，日期：{}）。本工具只能使用公开商业信息——\
                 个人信息来源在类型层面即被禁止。",
                personal.len(),
                personal.join("、")
            ));
        }
        let mut profile = profile;

        // ── 校验画像自身的一致性（**此前从未调用**）──
        //
        // ⚠️ 实测审计发现：`ParticipantProfile::validate()` 有完整实现
        // （角色块必须与声明角色一致、数值区间、期望值单调性…），
        // 而 `participant_profile_update` **从不调它** ⇒ 那些规则在
        // 真实路径上**全部失效**。
        //
        // 后果举例：声明 `vn_role: supplier` 却填 `Substitutor` 的字段块，
        // 会被**静默接受**——而下游会按错误角色口径解读那些数字
        // （供应商的"转换成本"与竞对的"攻击性"是完全不同的东西）。
        //
        // 这是"能力建了但没接"的又一例（第 9 处），且它漏在**校验层**：
        // 校验函数有测试（`shangyi-intel` 自己的测试全过），
        // 但生产路径不调用它 ⇒ 测试验证的是函数，不是**它的生效**。
        if let Err(e) = profile.validate() {
            return ExecOutcome::Failed(format!(
                "画像校验未通过：{e}。\n\
                 → 常见原因：`role_specific` 的字段块与 `value_net_role` 声明的角色不一致\
                 （例：声明 `supplier` 却给 `substitutor` 的字段）——\
                 那会让下游按错误角色口径解读这些数字。"
            ));
        }

        profile.updated_at_ms = self.now_ms();

        // ── 贝叶斯类型推断（§6.4）──
        //
        // ⚠️ 这一步此前**从未接线**：`infer_types` / `apply_inference`
        // 全仓零调用 ⇒ 画像的 `posterior_types` **永远是空的**，
        // 而报告里有 `most_likely_type()` 的展示位（它会显示"未推断"）、
        // KB 页面把"信息增益"列为产出物。**能力建了但没接上。**
        //
        // # 为什么它不能自动跑：似然模型没有合法来源
        //
        // `infer_types` 需要 `BehaviorModel`（各类型对各类行为的倾向权重），
        // 那是**领域判断**——"清库存型对手做促销的概率是长期战略型的几倍"。
        // 引擎不能凭空造它（那正是铁律 1 禁止的）。
        //
        // # 故的处置：让调用方**显式提供**，引擎负责计算
        //
        // 这与 `consequence_predict` 的分工一致：模型给结构与假设，
        // 引擎做数值与判定。没有 `behavior_models` 时**不推断**，
        // 并如实说明"缺似然模型，后验未更新"——而不是假装推断过。
        let mut inference_note: Option<serde_json::Value> = None;
        if let (Some(models_v), Some(priors_v)) = (
            args.get("behavior_models"),
            args.get("type_priors"),
        ) {
            let models: Vec<shangyi_intel::BehaviorModel> =
                match serde_json::from_value(models_v.clone()) {
                    Ok(m) => m,
                    Err(e) => {
                        return ExecOutcome::Failed(format!(
                            "`behavior_models` 解析失败：{e}。\n\
                             形状：[{{\"type_name\": \"清库存型\", \"event_weights\": {{\"price_cut\": 0.9}}, \
                             \"magnitude_sensitivity\": 1.0}}]"
                        ))
                    }
                };
            let priors: BTreeMap<String, f64> = match serde_json::from_value(priors_v.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return ExecOutcome::Failed(format!(
                        "`type_priors` 解析失败：{e}。形状：{{\"清库存型\": 0.4, \"长期战略型\": 0.6}}\
                         （**必须归一化**——预测先验本身就是一条要留档的判断）"
                    ))
                }
            };
            match shangyi_intel::infer_types(&profile.timeline, &models, &priors) {
                Ok(inf) => {
                    // ⚠️ `information_gain` 也必须被用上：它的用途是
                    // "判断本次推断是否真的带来了信息"——接近 0 意味着
                    // 观测对类型无区分度，而报告**必须如实说明**这一点
                    // （否则读者会以为后验是"算出来的结论"，而它其实≈先验）。
                    let gain = inf.information_gain();
                    inference_note = Some(serde_json::json!({
                        "posterior": inf.posterior,
                        "information_gain": gain,
                        "observations_used": profile.timeline.len(),
                        "model_incomplete": inf.model_incomplete,
                        "note": if gain < 0.05 {
                            "**本次推断几乎没有带来信息**（增益 < 0.05）——\
                             观测对类型区分度不足，后验≈先验。报告须如实说明\
                             「证据不足以区分类型」，不得把它当作有依据的类型判断。"
                        } else {
                            "后验由引擎按贝叶斯规则更新（似然来自你给的 behavior_models）。"
                        },
                    }));
                    shangyi_intel::apply_inference(&mut profile, &inf, self.now_ms());
                }
                Err(e) => {
                    return ExecOutcome::Failed(format!(
                        "类型推断失败：{e}。\n\
                         常见原因：先验未归一化、或某个有先验的类型缺对应的 behavior_model。"
                    ))
                }
            }
        }

        let id = profile.participant_id.clone();
        match self.state.profiles.iter_mut().find(|p| p.participant_id == id) {
            Some(existing) => *existing = profile,
            None => self.state.profiles.push(profile),
        }
        // ⚠️ `inference_note` 必须进输出——否则推断白算了。
        //
        // 我第一版算完就丢（只存了个 Option 没用），那会重演本项目里
        // 反复出现的"接线了一半"：计算做了、结果被丢掉，
        // 而调用方看到的输出与"没做过推断"完全一样。
        let mut out = serde_json::json!({
            "participant_id": id,
            "profiles_total": self.state.profiles.len(),
        });
        if let Some(o) = out.as_object_mut() {
            match inference_note {
                Some(note) => {
                    o.insert("type_inference".into(), note);
                }
                None => {
                    // **不推断时如实说明**，而不是静默省略——
                    // 静默会让调用方以为"这个画像没有类型可推断"，
                    // 而真相是"没给似然模型"。
                    o.insert(
                        "type_inference".into(),
                        serde_json::json!({
                            "status": "not_performed",
                            "reason": "未提供 `behavior_models` 与 `type_priors`                                       ⇒ 后验未更新（`posterior_types` 保持为空）。\n                                       似然模型是**领域判断**（各类型做各类行为的相对倾向），                                       引擎不能凭空造它（铁律 1）。若你需要类型后验，                                       请提供这两项——引擎负责贝叶斯更新与信息增益计算。",
                        }),
                    );
                }
            }
        }
        ExecOutcome::Ok(out.to_string())
    }

    // ── 辅助 ──

    fn research_task_plan(&mut self, args: &serde_json::Value) -> ExecOutcome {
        if self.state.research_task.is_some() {
            return ExecOutcome::Failed("本次请求字段已经登记；不能通过重建计划删掉未完成字段。请按已有字段登记证据或具体缺口。".into());
        }
        match crate::research_task::ResearchTask::plan(args, &self.state.task_request_text) {
            Ok(mut task) => {
                task.set_budget(self.state.retrieval_attempts.get(), 16);
                let output = task.report();
                self.state.research_task = Some(task);
                ExecOutcome::Ok(output.to_string())
            }
            Err(error) => ExecOutcome::Failed(error),
        }
    }

    fn research_task_update(&mut self, args: &serde_json::Value) -> ExecOutcome {
        let Some(task) = self.state.research_task.as_mut() else {
            return ExecOutcome::Failed("先按真实用户原话登记请求字段，再把实际取得的正文证据登记到对应字段。".into());
        };
        task.set_budget(self.state.retrieval_attempts.get(), 16);
        match task.update_with_receipts(
            args,
            &self.state.fetched_sources.borrow(),
            &self.state.structured_receipts,
        ) {
            Ok(output) => ExecOutcome::Ok(output.to_string()),
            Err(error) => ExecOutcome::Failed(error),
        }
    }

    fn research_document_lookup(&mut self, call: &ToolCall, phase: SessionPhase) -> ExecOutcome {
        let Some(task) = self.state.research_task.as_mut() else {
            return ExecOutcome::Failed("先登记本次请求字段，才能定向寻找已见官方文件的原文。".into());
        };
        let lookup = match task.prepare_document_lookup(&call.arguments, self.state.retrieval_attempts.get(), 16) {
            Ok(lookup) => lookup,
            Err(error) => return ExecOutcome::Failed(error),
        };
        let search_args = serde_json::json!({"query":lookup.query});
        let search = self.execute(&ToolCall {
            call_id:format!("{}:search",call.call_id), name:"web_search".into(), arguments:search_args.clone(),
        }, phase);
        let receipt = |name:&str, args:serde_json::Value, outcome:&ExecOutcome| {
            let outcome = match outcome {
                ExecOutcome::Ok(output) => serde_json::json!({"status":"ok","output":output}),
                ExecOutcome::Failed(error) => serde_json::json!({"status":"failed","error":error}),
                ExecOutcome::Blocked(detail) => serde_json::json!({"status":"blocked","detail":detail}),
                _ => serde_json::json!({"status":"failed","error":"网络工具没有返回可用的检索结果"}),
            };
            serde_json::json!({"name":name,"arguments":args,"outcome":outcome})
        };
        let mut receipts = vec![receipt("web_search",search_args,&search)];
        let candidates = self.state.research_task.as_ref().expect("lookup keeps its request plan").lookup_candidates(&lookup.query);
        if matches!(search,ExecOutcome::Ok(_)) {
            if let Some(url) = candidates.first() {
                let fetch_args = serde_json::json!({"url":url});
                let fetched = self.execute(&ToolCall {
                    call_id:format!("{}:fetch",call.call_id),name:"web_fetch".into(),arguments:fetch_args.clone(),
                },phase);
                receipts.push(receipt("web_fetch",fetch_args,&fetched));
            }
        }
        let lookup = match self.state.research_task.as_mut().expect("lookup keeps its request plan").finish_document_lookup(&lookup.query) {
            Ok(lookup)=>lookup,
            Err(error)=>return ExecOutcome::Failed(error),
        };
        ExecOutcome::Ok(serde_json::json!({
            "lookup":lookup,"retrieval_receipts":receipts,
            "research_task":self.state.research_task.as_ref().expect("lookup keeps its request plan").report(),
            "note":"以上为实际定向检索与抓取回执；没有命中或原文未取得的字段仍须保留缺口，不能伪造网址。",
        }).to_string())
    }

    fn select_task_scope(&mut self, args: &serde_json::Value, phase: SessionPhase) -> ExecOutcome {
        if phase != SessionPhase::Intake {
            return ExecOutcome::Failed("仅新任务的 intake 可以选择范围，不能用范围选择绕过既有咨询闸门".into());
        }
        let scope = match serde_json::from_value::<TaskScope>(args.get("scope").cloned().unwrap_or_default()) {
            Ok(scope) => scope,
            Err(_) => return ExecOutcome::Failed("scope 须为 research/cost/research_and_cost/consultation".into()),
        };
        if scope == TaskScope::Conditional {
            return ExecOutcome::Failed("条件比较不能作为开场范围来绕过咨询；只有真实证据缺口才能交付明确的条件分析。".into());
        }
        let Some(reason) = args.get("reason").and_then(|v|v.as_str()).filter(|s|!s.trim().is_empty()) else {
            return ExecOutcome::Failed("须根据用户原话说明选择此范围的 reason，不能默认扩大任务".into());
        };
        if scope == TaskScope::Consultation {
            if let Err(reason) = crate::consultation::check_playbook_supports_mode(self.mode, self.playbook) {
                return ExecOutcome::Failed(reason);
            }
        }
        self.state.task_scope = Some(scope);
        ExecOutcome::TaskScopeSelected { scope,reason:reason.to_string() }
    }

    fn complete_task(&self, args: &serde_json::Value, phase: SessionPhase) -> ExecOutcome {
        let Some(scope) = self.state.task_scope.filter(|s|matches!(s,TaskScope::Research|TaskScope::Cost|TaskScope::ResearchAndCost)) else {
            return ExecOutcome::Failed("complete_task 仅用于已选择范围的资料/费用任务；完整咨询必须完成原阶段与确认流程".into());
        };
        if phase != SessionPhase::Intake {
            return ExecOutcome::Failed("资料/费用任务只能在 intake 按范围交付，不能代替咨询归档".into());
        }
        let Some(summary) = args.get("summary").and_then(|v|v.as_str()).filter(|s|!s.trim().is_empty()) else {
            return ExecOutcome::Failed("summary 必须是直接给用户的非空最终答复".into());
        };
        if !scope.needs_research() && crate::task_scope::has_double_escaped_layout(summary) {
            return ExecOutcome::Failed(
                "summary 格式未通过：没有真正换行，却多次出现文字形式的 \\n\\n、\\n- 或表格换行，说明段落/Markdown被重复转义。请保持已核实事实、数值、来源和未知项，只修正最终答复格式后重新调用 complete_task：summary 使用真正的换行字符，由 JSON 序列化自然转义，不要预先写成双重转义文本。用户要求表格时须提交带真正换行的 Markdown 表格。不需要重做检索或计算，也不要改写用户原话或代码中的字面量；本次尚未交付。".into()
            );
        }
        let Some(raw_missing) = args.get("missing").and_then(|v|v.as_array()) else {
            return ExecOutcome::Failed("missing 必须显式列出未取得事项；确无缺口时传空数组".into());
        };
        let mut missing = Vec::new();
        let mut research = None;
        let mut delivered_summary = summary.to_string();
        let mut source_urls:Vec<String> = self.state.task_sources.iter().cloned().collect();
        if scope.needs_research() && !raw_missing.is_empty() {
            return ExecOutcome::Failed("研究缺口只能来自本次已登记请求字段；missing请传空数组，在research_task_update中登记对应字段的状态。费用不完整由实际计算回执自动保留，不另添用户没问的单位或门牌。".into());
        }
        for item in raw_missing {
            let Some(item) = item.as_str().filter(|s|!s.trim().is_empty()) else {
                return ExecOutcome::Failed("missing 每项必须是非空说明，不能用空值掩盖缺口".into());
            };
            missing.push(item.to_string());
        }
        if scope.needs_research() {
            if self.state.task_retrieval_attempts == 0 {
                return ExecOutcome::Failed("本任务尚无真实检索/抓取凭据；先实际查询，不能只写资料总结就交付".into());
            }
            let Some(plan)=self.state.research_task.as_ref() else {
                return ExecOutcome::Failed("本次资料查询尚未登记用户请求字段；先建立真实原文锚定的计划，并登记每项证据或具体缺口，再交付。".into());
            };
            let delivery=match plan.delivery() {Ok(delivery)=>delivery,Err(error)=>return ExecOutcome::Failed(error)};
            missing.extend(delivery.missing.iter().cloned());
            source_urls=delivery.source_urls.clone();
            delivered_summary=crate::research_task::render_delivery(&delivery);
            research=Some(serde_json::to_value(delivery).expect("research delivery is serializable"));
        }
        if scope.needs_cost() {
            if self.state.task_costs.is_empty() {
                return ExecOutcome::Failed("本任务尚无成功的 business_cost 结果；费用必须由计算工具产生，未知金额用 null".into());
            }
            for cost in self.state.task_costs.values() {
                if cost["amounts_complete"] != true || cost["cash_classification_complete"] != true || cost["costs_complete"] != true {
                    missing.push(format!("方案「{}」仅计算已知费用；未取得金额、未知性质或未列费用仍需保留",cost["scenario"].as_str().unwrap_or("未命名")));
                }
            }
            if scope.needs_research() {
                delivered_summary.push_str(&crate::delivery_render::render_cost_receipts(&self.state.task_costs.values().cloned().collect::<Vec<_>>()));
            }
        }
        missing.sort();missing.dedup();
        ExecOutcome::ScopedTaskCompleted(ScopedTaskResult {
            scope,status:if missing.is_empty(){TaskResultStatus::Complete}else{TaskResultStatus::Partial},
            summary:delivered_summary,missing,
            source_urls,
            retrieval_attempts:self.state.task_retrieval_attempts,
            cost_scenarios:self.state.task_costs.values().cloned().collect(),
            conditional:None,
            research,
        })
    }

    fn build_conditional_delivery(&mut self, trigger:ConditionalTrigger)->Result<ScopedTaskResult,String> {
        let attempted=self.state.attempted_model.clone().or_else(||self.state.game_spec_evidence.clone())
            .or_else(||self.state.game_spec.as_ref().map(|spec|serde_json::to_value(spec).expect("game serialization")));
        let mut gaps=self.state.decision_gaps.clone();
        if gaps.is_empty() {gaps=self.payoff_evidence_gaps(attempted.as_ref().unwrap_or(&serde_json::Value::Null));}
        let mut costs=Vec::new();
        for scenario in self.state.cost_scenarios.values() {
            let mut scenario=scenario.clone();
            let attributes:Vec<ConstraintAttribute>=serde_json::from_value(scenario["constraint_attributes"].clone())
                .map_err(|error|format!("旧费用方案缺少可校验属性：{error}；须重新计算，不能默认通过底线"))?;
            let assessment=self.state.constraints.assess(&attributes);
            if assessment.status==Acceptance::Unknown {
                gaps.push(EvidenceGap{reason:EvidenceGapReason::MissingDecisionEvidence,participant_id:Some("user".into()),profile:None,
                    detail:format!("方案「{}」仍有无法核对的上限或关键属性，不能把它当作可推荐选项",scenario["scenario"].as_str().unwrap_or("未命名"))});
            }
            scenario["comparison_only"]=serde_json::json!(assessment.status!=Acceptance::Eligible);
            scenario["constraint_assessment"]=serde_json::json!(assessment);
            scenario["constraint_review"]=self.state.constraints.snapshot();
            if scenario["amounts_complete"]!=true || scenario["cash_classification_complete"]!=true {
                gaps.push(EvidenceGap{reason:EvidenceGapReason::MissingDecisionEvidence,participant_id:Some("user".into()),profile:None,
                    detail:format!("方案「{}」仍有未取得金额或未确定的费用性质，不能以零补齐",scenario["scenario"].as_str().unwrap_or("未命名"))});
            }
            costs.push(scenario);
        }
        gaps.sort_by(|a,b|(&a.reason,&a.participant_id,&a.profile,&a.detail).cmp(&(&b.reason,&b.participant_id,&b.profile,&b.detail)));
        gaps.dedup();
        let source_urls:std::collections::BTreeSet<String>=self.state.fetched_pages.borrow().values()
            .filter_map(|page|page["url"].as_str().map(str::to_string)).collect();
        let snapshot=ConditionalSnapshot{
            question:self.state.task_request_text.clone(),attempted_model:attempted,gaps:gaps.clone(),
            cost_scenarios:costs,source_urls:source_urls.into_iter().collect(),retrieval_attempts:self.state.task_retrieval_attempts,
            constraint_evaluation:Some(self.state.constraints.snapshot()),
        };
        let result=crate::conditional::build_conditional_result(&snapshot,trigger)?;
        self.state.decision_gaps=gaps;
        self.state.task_scope=Some(TaskScope::Conditional);
        Ok(result)
    }

    fn conditional_analysis(&mut self,args:&serde_json::Value)->ExecOutcome {
        if !args.as_object().is_some_and(|object|object.keys().all(|key|key=="reason"))
            || args.get("reason").is_some_and(|value|!value.is_string()) {
            return ExecOutcome::Failed("conditional_analysis只接受可选reason说明意图；真实快照、缺口和数字由引擎读取，不能由模型填写。".into());
        }
        match self.build_conditional_delivery(ConditionalTrigger::ExplicitRequest) {
            Ok(result)=>ExecOutcome::ScopedTaskCompleted(result),
            Err(reason)=>ExecOutcome::Failed(reason),
        }
    }

    fn equilibrium_ids(&self) -> Result<Vec<String>, ExecOutcome> {
        let Some(json) = &self.state.equilibria_json else {
            return Err(ExecOutcome::Failed(
                "会话内尚无解集。请先调用 `solve_equilibria`。".into(),
            ));
        };
        let v: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| ExecOutcome::Failed(format!("解集 JSON 损坏：{e}")))?;
        let ids = v
            .get("equilibria")
            .and_then(|e| e.as_array())
            .map(|a| {
                a.iter()
                    .enumerate()
                    .map(|(i, e)| {
                        e.get("id")
                            .and_then(|x| x.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("e{}", i + 1))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if ids.is_empty() {
            return Err(ExecOutcome::Failed(
                "解集为空——空解集不得被当作「求解成功」。请检查求解器是否被拒绝。".into(),
            ));
        }
        Ok(ids)
    }
    fn request_followup_task(&self, args: &serde_json::Value) -> ExecOutcome {
        let Some(obj) = args.as_object() else {
            return ExecOutcome::Failed("begin_followup_task 参数必须是 JSON 对象".into());
        };
        if let Some(bad_key) = obj.keys().find(|k| k.as_str() != "reason") {
            return ExecOutcome::Failed(format!(
                "begin_followup_task 不支持未知字段 `{bad_key}`（仅接受可选 `reason`）"
            ));
        }
        let reason = match obj.get("reason") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(s)) => {
                let t = s.trim();
                if t.is_empty() {
                    return ExecOutcome::Failed("`begin_followup_task.reason` 若传入则必须是非空字符串".into());
                }
                Some(t.to_string())
            }
            Some(_) => {
                return ExecOutcome::Failed("`begin_followup_task.reason` 必须是字符串".into());
            }
        };
        if self.state.task_scope.is_none() {
            return ExecOutcome::Failed("当前任务尚未交付或未选择范围，不能重置新一轮任务范围".into());
        }
        ExecOutcome::FollowupTaskRequested { reason }
    }

    pub fn clear_artifact_keys(&mut self, keys: &[String]) {
        for key in keys {
            match key.as_str() {
                "participant_profiles" => self.state.profiles.clear(),
                "gamespec" => {
                    self.state.game_spec = None;
                    self.state.game_spec_evidence = None;
                }
                "equilibrium_set" => self.state.equilibria_json = None,
                "equilibrium_distribution" => {
                    self.state.distribution = None;
                    self.state.selector_scores.clear();
                }
                "consequence_report" => self.state.consequence_report = None,
                "discovery_report" => self.state.discovery_report = None,
                "recommendation" => self.state.recommendation = None,
                "report_model" => {
                    self.state.report_json = None;
                    self.state.report_model = None;
                }
                "case_archived" => self.state.case_library = shangyi_kb::CaseLibrary::new(),
                _ => {}
            }
        }
    }

    pub fn seed_followup_request_text(&mut self, text: &str) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            if !self.state.task_request_text.is_empty() {
                self.state.task_request_text.push('\n');
            }
            self.state.task_request_text.push_str(trimmed);
        }
    }
}

impl ToolExecutor for DomainExecutor<'_> {
    fn set_task_scope(&mut self, scope: Option<TaskScope>) {
        Self::set_task_scope(self, scope);
    }

    fn clear_artifact_keys(&mut self, keys: &[String]) {
        Self::clear_artifact_keys(self, keys);
    }

    fn record_user_input(&mut self,text:&str,actor:&str) {self.ingest_user_input(text,actor);}

    fn take_invalidated_artifacts(&mut self)->Vec<ArtifactKind> {
        std::mem::take(&mut self.state.constraint_invalidations)
    }

    fn conditional_fallback(&mut self,gaps:&[EvidenceGap])->Result<ScopedTaskResult,String> {
        if gaps.is_empty() || gaps.iter().any(|gap|!self.state.decision_gaps.contains(gap)) {
            return Err("重复无进展转条件交付必须来自本执行器实际记录的证据缺口，不能注入替代缺口。".into());
        }
        self.build_conditional_delivery(ConditionalTrigger::RepeatedNoProgress)
    }

    fn execute(&mut self, call: &ToolCall, phase: SessionPhase) -> ExecOutcome {
        if self.state.task_scope.is_some_and(|scope|!scope.allows_tool(&call.name)) {
            return ExecOutcome::Failed("该工具超出本次已选择的资料/费用任务范围，不能强制追加博弈与确认流程".into());
        }
        let outcome = match call.name.as_str() {
            "select_task_scope" => self.select_task_scope(&call.arguments, phase),
            "begin_followup_task" => self.request_followup_task(&call.arguments),
            "research_task_plan" => self.research_task_plan(&call.arguments),
            "research_task_update" => self.research_task_update(&call.arguments),
            "research_document_lookup" => self.research_document_lookup(call, phase),
            "complete_task" => self.complete_task(&call.arguments, phase),
            "vn_map" => self.vn_map(&call.arguments),
            "checkpoint" => self.checkpoint(&call.arguments, phase),
            "pars_check" => self.pars_check(&call.arguments),
            "request_rewind" => self.request_rewind(&call.arguments, phase),
            "case_store_read" => self.case_store_read(&call.arguments),
            "case_store_write" => self.case_store_write(&call.arguments),
            "report_render" => self.report_render(&call.arguments),
            "research_record" => match crate::business::research_record(&call.arguments, &self.state.fetched_sources.borrow()) {
                Ok(record) => {
                    if let Some(existing) = self.state.research_records.iter_mut().find(|r| crate::business::same_observed_candidate(r, &record)) {
                        let url = record["url"].as_str().unwrap_or_default();
                        let urls = existing["source_urls"].as_array_mut().expect("research_record always sets source_urls");
                        if !urls.iter().any(|v| v.as_str() == Some(url)) { urls.push(serde_json::json!(url)); }
                    } else {
                        self.state.research_records.push(record);
                    }
                    ExecOutcome::Ok(serde_json::json!({"records":self.state.research_records}).to_string())
                },
                Err(e) => ExecOutcome::Failed(e),
            },
            "constraint_register" => self.constraint_register(&call.arguments),
            "conditional_analysis" => self.conditional_analysis(&call.arguments),
            "constraint_check" => self.constraint_check(&call.arguments),
            "business_cost" => self.business_cost(&call.arguments),
            "ask_user" => self.ask_user(&call.arguments),
            "gamespec_build" => self.gamespec_build(&call.arguments),
            "gamespec_validate" => self.gamespec_validate(&call.arguments),
            "solve_equilibria" => self.solve_equilibria(&call.arguments),
            "select_equilibria" => self.select_equilibria(&call.arguments),
            "select_fuse" => self.select_fuse(&call.arguments),
            "swarm_run" => self.swarm_run(&call.arguments),
            "data_ingest" => self.data_ingest(&call.arguments),
            "sandbox_exec" => self.sandbox_exec(&call.arguments),
            "kb_retrieve" => self.kb_retrieve(&call.arguments),
            "monte_carlo" => self.monte_carlo(&call.arguments),
            "sensitivity_scan" => self.sensitivity_scan(&call.arguments),
            "consequence_predict" => self.consequence_predict(&call.arguments),
            "preference_inject" => self.preference_inject(&call.arguments),
            "recommend_build" => self.recommend_build(&call.arguments),
            "novel_move_filter" => self.novel_move_filter(&call.arguments),
            "discover_satisfice" => self.discover_satisfice(&call.arguments),
            "poi_survey" => self.poi_survey(call, &call.arguments),
            "web_search" => self.web_search(&call.arguments),
            "web_fetch" => self.web_fetch(&call.arguments),
            "participant_profile_update" => self.participant_profile_update(&call.arguments),
            // 兜底：**当前没有任何工具会走到这里**（全部 39 个工具均已接线，
            // 由 `all_tools_are_wired` 断言）。保留它是因为这两件事都不该发生
            // 却都可能发生：
            //
            // 1. 有人在工具注册表里加了新工具，但忘了在这张派发表里实现它
            // 2. 模型调用了一个根本不存在的工具名
            //
            // 两者的正确处置相同：**如实说"未实现"或"未注册"，并报出名字**。
            // 绝不能静默返回空结果——那会让"工具不存在"表现为"工具跑了但没输出"。
            other => not_wired(
                other,
                wiring_status().get(other).copied().unwrap_or("未注册于接线清单"),
            ),
        };
        if matches!(call.name.as_str(),"web_search"|"web_fetch"|"poi_survey") {
            match &outcome {
                ExecOutcome::Ok(output) => {
                    self.state.task_retrieval_attempts += 1;
                    if call.name == "web_fetch" {
                        let page:serde_json::Value=serde_json::from_str(output).expect("successful web_fetch emits JSON");
                        if let Some(url)=page.get("url").and_then(|v|v.as_str()) { self.state.task_sources.insert(url.to_string()); }
                        if let Some(task) = self.state.research_task.as_mut() {
                            task.observe_fetch(call.arguments["url"].as_str().unwrap_or_default(),page["url"].as_str(),true);
                        }
                    }
                }
                ExecOutcome::Blocked(reason) => {
                    self.state.task_retrieval_attempts += 1;
                    self.state.task_gaps.push(reason.clone());
                    if call.name == "web_fetch" {
                        if let Some(task) = self.state.research_task.as_mut() {
                            task.observe_fetch(call.arguments["url"].as_str().unwrap_or_default(),None,false);
                        }
                    }
                }
                _ => {},
            }
            if let Some(task) = self.state.research_task.as_mut() {
                task.set_budget(self.state.retrieval_attempts.get(),16);
            }
        } else if call.name == "business_cost" {
            if let ExecOutcome::Ok(output)=&outcome {
                let cost:serde_json::Value=serde_json::from_str(output).expect("successful business_cost emits JSON");
                let id=cost["scenario_id"].as_str().expect("cost scenario has identity").to_string();
                self.state.task_costs.insert(id,cost);
            }
        }
        outcome
    }

    fn begin_followup_task(&mut self) {
        if self.state.task_scope==Some(TaskScope::Conditional) {
            self.invalidate_constraint_dependents();
            self.state.attempted_model=None;
            self.state.decision_gaps.clear();
        }
        self.state.research_task=None;
        self.state.task_request_text.clear();
        self.state.task_scope=None;
        self.state.task_retrieval_attempts=0;
        self.state.task_sources.clear();
        self.state.task_costs.clear();
        self.state.task_gaps.clear();
        self.state.retrieval_attempts.set(0);
        self.state.fetch_failures.borrow_mut().clear();
    }

    fn artifacts(&self) -> Vec<(ArtifactKind, String)> {
        let mut out = Vec::new();
        if !self.state.profiles.is_empty() {
            out.push((
                ArtifactKind::ParticipantProfiles,
                format!("profiles:{}", self.state.profiles.len()),
            ));
        }
        if let Some(s) = &self.state.game_spec {
            out.push((
                ArtifactKind::GameSpecValidated,
                format!("gamespec:{}:{}", s.game_id, fingerprint_str(&serde_json::to_string(s).expect("GameSpec is serializable"))),
            ));
        }
        if let Some(e) = &self.state.equilibria_json {
            out.push((
                ArtifactKind::EquilibriumSet,
                format!("eq:{}", fingerprint_str(e)),
            ));
        }
        if let Some(d) = &self.state.distribution {
            out.push((
                ArtifactKind::SelectionOutput,
                format!("dist:{}", d.entries.len()),
            ));
        }
        // ⚠️ 这两个是**阶段图要求的产物**，必须登记——否则
        // `consequence → recommend` 与 `discover → recommend` 结构上不可达。
        //
        // 缺陷背景（与 recommend_build 未注册、report_json 未写同源）：
        // `consequence_predict` / `discover_satisfice` 都**成功了**，
        // 但算出的结果没被登记为产物 ⇒ 闸门拦死。
        // 这类缺陷只有"对照阶段图逐项检查"才能发现——单元测试各自都过。
        if let Some(c) = &self.state.consequence_report {
            out.push((
                ArtifactKind::ConsequenceReport,
                format!(
                    "consequence:{}:{}",
                    c.decision,
                    fingerprint_str(&c.reactions.len().to_string())
                ),
            ));
        }
        // archived 阶段的交付物：**案例库里已有冻结预测的案例**。
        //
        // 这是它唯一的登记来源——`case_store_write` 成功后库非空。
        // 有了它，runner 才能在 archived 阶段判断"归档做完了"并收尾
        // （此前 `produces` 是空集 ⇒ 该阶段**没有终止条件**）。
        if !self.state.case_library.is_empty() {
            out.push((
                ArtifactKind::CaseArchived,
                format!("archived:{}", self.state.case_library.len()),
            ));
        }
        if let Some(d) = &self.state.discovery_report {
            out.push((
                ArtifactKind::DiscoveryReport,
                format!(
                    "discovery:{}:{}",
                    d.satisfied.len(),
                    fingerprint_str(
                        &d.satisfied
                            .iter()
                            .map(|e| e.candidate.id.clone())
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                ),
            ));
        }
        if let Some(rec) = &self.state.recommendation {
            out.push((
                ArtifactKind::RecommendationPack,
                format!("recommendation:{}", fingerprint_str(rec)),
            ));
        }
        if let Some(r) = &self.state.report_json {
            out.push((
                ArtifactKind::ReportBundle,
                format!("report:{}", fingerprint_str(r)),
            ));
        }
        out
    }
}

/// 未接线工具的**统一**响应。
///
/// 单独抽出来是为了让"未接线"这件事**可被搜索、可被计数**——
/// 若每个分支各写各的字符串，审计时无法回答"还有哪些工具没接线"。
fn not_wired(tool: &str, reason: &str) -> ExecOutcome {
    ExecOutcome::Failed(format!(
        "工具 `{tool}` 尚未接线：{reason}。这不是外部依赖问题，是实现未完成——\
         请勿用推测结果替代。"
    ))
}

/// 接线清单里处于"已接线"的标记集合（供测试判别，避免各处硬编码字符串）。
pub fn wired_statuses() -> [&'static str; 3] {
    ["wired", "wired-needs-network", "wired-by-loop"]
}

fn parse_selector(s: &str) -> Option<SelectorId> {
    SelectorId::ALL.iter().copied().find(|x| x.as_str() == s)
}

fn render_validation(errs: &[ValidationError]) -> String {
    let lines: Vec<String> = errs.iter().map(|e| format!("- {e}")).collect();
    let mut out = format!("GameSpec 校验未通过（{} 项）：\n{}", errs.len(), lines.join("\n"));

    // ── 「未确认参数」必须给出**可执行的出路**，否则模型会原地打转 ──
    //
    // # 实测（negociation/scenario，2026-09-19）
    //
    // 模型把六个拿不准的数值诚实地标成 `assumed`（占位值，注明了"无来源待替换"），
    // 然后反复重发——**61 次 `gamespec_build`，60 次失败**，单 turn 直接撞上
    // 64 步上限、整个会话作废。它三次试图绕开（改 `params` 为 `"{}"`、
    // 把整个 spec 塞进字符串、换 `game_id`），全都不解决根因。
    //
    // 根因**不在模型**：`assumed` 是它唯一能表达"这个数我没底"的方式，
    // 而这条路在设计上是**死路**（铁律 2 要求走 checkpoint）。可原先的报错
    // 只说"不得进入求解"，**没说那该怎么办**——于是它试遍了所有写法，
    // 唯独没试那条唯一有效的（先摆给用户确认）。
    //
    // 通用教训：**当唯一正确路径不在模型的直觉里时，报错必须把它写出来。**
    // "你不能这么做"是半个错误信息；"你该那么做"才是完整的一个。
    if errs
        .iter()
        .any(|e| matches!(e, ValidationError::UnconfirmedParam(_)))
    {
        out.push_str(
            "\n\n**→ 下一步（唯一出路）**：把这些参数摆到**用户**面前确认，\
             而不是把它们标成 `assumed` 就交差。\n\
             1. 调 `checkpoint`，把每个待定假设写成一条 `items` \
                （`content` 说明假设什么、`proposed_value` 给出数值、\
                `source` 写你据以估计的依据）\n\
             2. 用户批准后（cp2 由本阶段持有），**重新调 `gamespec_build`**，\
                把那些参数的 `source` 改成 \
                `{\"kind\":\"confirmed_assumption\",\"checkpoint\":\"cp2\",\"basis\":\"保留原始假设依据\"}`\n\
             3. 那时校验才会通过——本工具**不能**替你跳过确认\
                （铁律 2：没有来源的数值不得进入模型）\n\
             \n\
             ⚠️ 注意「标成 assumed」与「摆给用户确认」的区别：\
             前者是**诚实但无效**的（数值进了不了模型，分析做不下去），\
             后者才是本阶段的正解。**有依据的估计不是编造**——\
             接受情景假设不代表已经证实；仍须保留依据和假设性质，不能改写为用户提供的经营事实。",
        );
    }
    if errs
        .iter()
        .any(|e| matches!(e, ValidationError::PayoffCellsAllMissing(_)))
    {
        out.push_str(
            "\n\n**→ `payoffs.values.cells` 不能为空 `{}`**：即使是分步搭建的骨架，首次提交也须包含各策略组合的\
             初始字面量支付与空 `params: {}`（之后再带 `merge: true` 增量补参数与引用）；\
             若支付数字本身因缺少关键事实而未知，须先核实或改用 `conditional_analysis` 交付条件分析。",
        );
    }
    out
}

fn summarize_spec(spec: &GameSpec) -> String {
    serde_json::json!({
        "game_id": spec.game_id,
        "players": spec.players.iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
        "strategy_counts": spec.strategies.iter()
            .map(|(k, v)| (k.clone(), v.len()))
            .collect::<BTreeMap<_, _>>(),
        "params": spec.params.len(),
        "validated": true,
    })
    .to_string()
}

/// 对独立 GameSpec 执行确定性求解 + 可选精炼，产出解集 pretty JSON。
///
/// # 为什么是自由函数（而不留在 `DomainExecutor` 方法里）
///
/// `shangyi solve`（离线求解子命令）必须与工具路径 `solve_equilibria` 走
/// **同一段代码**：解集口径只允许有一份。若 CLI 侧另拼一份等价逻辑，
/// 两处会各自漂移——那正是本仓库明令禁止的知识重复。
///
/// `args` 与工具参数同构：`method`（求解方法）、`refine`（精炼方法）、
/// `refine_denominator`（ESS 扰动格点分辨率，2..=64）。未知键被忽略。
pub fn solve_spec_to_json(spec: &GameSpec, args: &serde_json::Value) -> Result<String, String> {
    let game = match NormalFormGame::from_gamespec(spec) {
        Ok(g) => g,
        Err(e) => return Err(format!("GameSpec 无法适配为正规形博弈：{e}")),
    };

    // 方法由参数指定；默认支持枚举（唯一完备路径）
    let mut req = SolveRequest::new(game);
    if let Some(m) = args.get("method").and_then(|v| v.as_str()) {
        use shangyi_solver::SolveMethod;
        req = match m {
            "support_enumeration" | "support" => req.with_method(SolveMethod::SupportEnumeration),
            "lemke_howson" | "lh" => req.with_method(SolveMethod::LemkeHowson),
            "analytic" => req.with_method(SolveMethod::Analytic),
            other => {
                return Err(format!(
                    "未知求解方法 `{other}`；可用：support_enumeration / lemke_howson / analytic。"
                ))
            }
        };
    }

    match shangyi_solver::NativeSolver.solve(req) {
        Ok(sol) => {
            // ── 精炼过滤（§5.3：`[solve] 均衡枚举 + 精炼 → 解集`）──
            //
            // 这一步的语义需要说清楚（否则容易被误读）：
            // - **枚举**：找全部均衡（`solve_support_enumeration`）
            // - **精炼**：用更强的解概念**筛掉**不稳健的那些
            //
            // 精炼是**可选的**（`args.refine`）：它缩小解集，而"是否要
            // 用更强的解概念"取决于问题——有时你想看到全部均衡
            // （然后由 select 阶段的六判据决定），有时你只想要稳健的。
            // 故默认**不做**精炼，但**明确报告**用了哪种。
            let mut refined_note: Option<serde_json::Value> = None;
            if let Some(method) = args.get("refine").and_then(|v| v.as_str()) {
                let game = match NormalFormGame::from_gamespec(spec) {
                    Ok(g) => g,
                    Err(e) => return Err(format!("精炼需要博弈矩阵，但它无法适配：{e}")),
                };
                // 扰动格点分辨率：ESS 的判据要在"所有可能的扰动 τ"上验证，
                // 而那是个连续集合 ⇒ 用分母为 N 的有理格点逼近。
                // 默认 6（比单测用的 3 更细），可显式覆盖。
                let den = args
                    .get("refine_denominator")
                    .and_then(|v| v.as_i64())
                    .filter(|n| (2..=64).contains(n))
                    .unwrap_or(6) as i128;
                let r = match method {
                    "trembling_hand_perfect" | "thp" => {
                        shangyi_solver::refine::trembling_hand_perfect(&game, &sol.equilibria)
                    }
                    // `ess_refinement` 是 ESS 的**批量入口**：一个函数覆盖
                    // 单种群与双种群两种读法（用 `symmetric_reading` 切换）。
                    // 直接调 `ess_symmetric` 是错的——它只判**单个** σ。
                    "ess_symmetric" | "ess" => {
                        shangyi_solver::refine::ess_refinement(&game, &sol.equilibria, true, den)
                    }
                    "ess_two_population" | "ess_two" => {
                        shangyi_solver::refine::ess_refinement(&game, &sol.equilibria, false, den)
                    }
                    other => {
                        return Err(format!(
                            "未知精炼方法 `{other}`。可用：trembling_hand_perfect（颤抖手完美）、\
                             ess_symmetric（对称演化稳定）、ess_two_population（双种群演化稳定）。\n\
                             **未实现**（如实列出，不要试）：{}",
                            shangyi_solver::refine::not_implemented_refinements().join("；")
                        ))
                    }
                };
                match r {
                    Ok(res) => {
                        // 精炼掉全部均衡时**必须响亮报告**：那说明该解概念与本博弈
                        // 不相容（例如单种群 ESS 用在非对称博弈上）——
                        // 而不是"没有均衡"。
                        refined_note = Some(serde_json::json!({
                            "method": res.method,
                            "candidates": res.candidates.len(),
                            "survivors": res.survivors
                                .iter()
                                .map(|i| format!("e{}", i + 1))
                                .collect::<Vec<_>>(),
                            "excluded": res.reasons.iter().map(|(i, why)| {
                                serde_json::json!({
                                    "equilibrium": format!("e{}", i + 1),
                                    "reason": why
                                })
                            }).collect::<Vec<_>>(),
                        }));
                        if res.survivors.is_empty() && !sol.equilibria.is_empty() {
                            return Err(format!(
                                "精炼（{}）**筛掉了全部 {} 个均衡**。\
                                 这不等于「没有均衡」——它的意思是：\
                                 本博弈里没有该解概念下的稳定解。\
                                 常见原因：把单种群 ESS 判据用在**非对称**博弈上。\
                                 请改用 `trembling_hand_perfect`，或不加精炼（看全部均衡）。",
                                res.method, sol.equilibria.len()
                            ));
                        }
                    }
                    Err(e) => {
                        // 错误信息必须**可操作**：solver 的类型化拒绝
                        // （如"单种群 ESS 只对对称博弈定义"）本身就说清了原因，
                        // 但读者还需要知道**下一步该做什么**——
                        // 读者很容易把"判据不适用"读成"这个博弈没有稳定解"。
                        let hint = match method {
                            "ess_symmetric" | "ess" => {
                                "\n→ 单种群 ESS 只对**对称**博弈有定义\
                                 （判据不适用**不等于**「本博弈没有均衡」）。\
                                 若本博弈是非对称的（各方策略集不同），\
                                 请改用 `ess_two_population`（双种群）\
                                 或 `trembling_hand_perfect`（颤抖手完美，对非对称也适用）。"
                            }
                            _ => {
                                "\n→ 该解概念与本博弈不相容，**不等于**「没有均衡」。\
                                 请改用其他精炼方法，或不加精炼（看全部均衡）。"
                            }
                        };
                        return Err(format!("精炼（{method}）被类型化拒绝：{e}{hint}"));
                    }
                }
            }

            let mut payload = sol_json(&sol);
            if let Some(r) = refined_note {
                if let Some(o) = payload.as_object_mut() {
                    o.insert("refinement".into(), r);
                }
            }
            serde_json::to_string_pretty(&payload).map_err(|e| format!("解集序列化失败：{e}"))
        }
        // **不静默降级**：退化/超范围一律类型化上报，绝不返回空解集冒充"无均衡"
        Err(e) => Err(format!(
            "求解器拒绝（{e}）。这是类型化拒绝，不是「没有均衡」——\
             请检查博弈定义，或如需近似请明确改用其他方法。",
        )),
    }
}

fn sol_json(sol: &shangyi_solver::Solution) -> serde_json::Value {
    serde_json::json!({
        "route": format!("{:?}", sol.route),
        "completeness": format!("{:?}", sol.completeness),
        "equilibria": sol.equilibria.iter().enumerate().map(|(i, e)| {
            serde_json::json!({
                "id": format!("e{}", i + 1),
                "kind": format!("{:?}", e.kind),
                "support": e.support,
                // 混合策略用**精确有理数**的字符串形式（`Rational` 的 Display），
                // 不经 f64 中转——否则"精确均衡"的称号在序列化这一步就丢了
                "strategy": e.strategy.iter()
                    .map(|row| row.iter().map(|p| p.to_string()).collect::<Vec<_>>())
                    .collect::<Vec<_>>(),
                "payoffs": e.payoffs.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
    })
}

/// 截断到不超过 `max` 字节且不切坏 UTF-8。
fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…（已截断，完整内容见 artifacts）", &s[..end])
}

fn fingerprint_str(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// 供外部查看"哪些工具已接线"的清单（自检与文档生成用）。
pub fn wiring_status() -> BTreeMap<&'static str, &'static str> {
    use std::collections::BTreeMap;
    BTreeMap::from([
        ("select_task_scope", "wired"),
        ("begin_followup_task", "wired"),
        ("complete_task", "wired"),
        ("research_task_plan", "wired"),
        ("research_task_update", "wired"),
        ("research_document_lookup", "wired"),
        ("research_record", "wired"),
        ("constraint_register", "wired"),
        ("conditional_analysis", "wired"),
        ("constraint_check", "wired"),
        ("business_cost", "wired"),
        ("gamespec_build", "wired"),
        ("gamespec_validate", "wired"),
        ("solve_equilibria", "wired"),
        ("select_equilibria", "wired"),
        ("select_fuse", "wired"),
        ("kb_retrieve", "wired"),
        ("monte_carlo", "wired"),
        ("sensitivity_scan", "wired"),
        ("novel_move_filter", "wired"),
        ("discover_satisfice", "wired"),
        ("participant_profile_update", "wired"),
        // 抓取已实现真实后端（robots + 限速）；检索需要配置检索后端
        ("web_search", "needs-external"),
        // 周边计数已实现真实后端；未配 AMAP_KEY 时返回 Blocked（未取得）
        ("poi_survey", "needs-external"),
        ("web_fetch", "wired-needs-network"),
        ("vn_map", "wired"),
        ("pars_check", "wired"),
        ("ask_user", "wired"),
        ("data_ingest", "wired"),
        ("swarm_run", "wired"),
        ("consequence_predict", "wired"),
        ("preference_inject", "wired"),
        ("recommend_build", "wired"),
        ("report_render", "wired"),
        ("case_store_read", "wired"),
        ("case_store_write", "wired"),
        ("checkpoint", "wired"),
        ("request_rewind", "wired"),
        ("sandbox_exec", "wired"),
    ])
}



#[cfg(test)]
mod diagnose_tests {
    use super::diagnose_unbalanced_json;
    /// **不平衡的 JSON 要说清缺什么**（实测：同一形状失败 4 次）。
    ///
    /// 旧信息只给解析器位置（"column 3473"），而模型在 2547 个字符里
    /// 数不出括号 ⇒ 它只能重写，很可能再错。本测试锁住"给出缺口计数"。
    #[test]
    fn reports_missing_closers() {
        let broken = r#"{"a": {"b": [1, 2}"#;
        let d = diagnose_unbalanced_json(broken);
        assert!(d.contains("不完整"), "应说明不完整：{d}");
        assert!(
            d.contains("缺 1 个 `}`") && d.contains("1 个 `]`"),
            "应给出具体缺口数量：{d}"
        );
        assert!(
            d.contains("分两步") || d.contains("最小可解"),
            "应给出可操作的替代做法（重写通常仍错在同一处）：{d}"
        );
    }

    #[test]
    fn reports_excess_closers() {
        let extra = r#"{"a": 1}}}"#;
        let d = diagnose_unbalanced_json(extra);
        assert!(d.contains("多出"), "应说明多出闭合：{d}");
    }

    #[test]
    fn reports_balanced_but_invalid() {
        // 括号平衡但缺逗号
        let bad = r#"{"a": 1 "b": 2}"#;
        let d = diagnose_unbalanced_json(bad);
        assert!(
            d.contains("括号平衡") && d.contains("缺逗号"),
            "应指出问题不在括号：{d}"
        );
    }

    /// 字符串里的括号**不该被计数**（否则诊断会给错的数量）。
    #[test]
    fn ignores_braces_inside_strings() {
        // 这个 JSON 是平衡的：字符串里的 }) 不应干扰计数
        let ok = r#"{"note": "用了 }) 作为分隔", "v": 1}"#;
        let d = diagnose_unbalanced_json(ok);
        assert!(
            d.contains("括号平衡"),
            "字符串里的括号不应被计入（实测踩过这类误判）：{d}"
        );
    }

    /// 转义引号不该让扫描器"跑出字符串"。
    #[test]
    fn handles_escaped_quotes() {
        let ok = r#"{"note": "他说\"好的\"", "v": 1}"#;
        let d = diagnose_unbalanced_json(ok);
        assert!(d.contains("括号平衡"), "转义引号处理后应判为平衡：{d}");
    }
}

/// GameSpec 输入归一化的回归测试。
///
/// **全部形状取自一次真实咨询的日志**（`case-20260919-scenario-063033`：
/// 63 次 `gamespec_build`，1 次成功、62 次失败，单 turn 撞上 64 步上限
/// 导致整个会话作废）。每个测试对应一个实际出现过的失败形状——
/// 它们是"接口不该要求模型知道我们的内部表示"这条原则的回归网。
#[cfg(test)]
mod spec_normalize_tests {
    use super::{normalize_spec_input, GameSpec};

    /// 最小可用的 spec（含一个玩家、一个策略、一个支付格）。
    fn minimal() -> serde_json::Value {
        serde_json::json!({
            "game_id": "t",
            "players": [
                {"id": "us", "name": "我方", "vn_role": "focal", "relation_to_game": "player"},
                {"id": "them", "name": "对方", "vn_role": "substitutor", "relation_to_game": "player"}
            ],
            "strategies": {"us": ["进"], "them": ["挡"]},
            "payoffs": {"form": "normal", "values": {"cells": {"进,挡": {"us": 1, "them": 2}}}},
            "params": {}
        })
    }

    /// 归一化后必须能被反序列化成 `GameSpec`——这才是"真的修好了"。
    fn assert_builds(v: serde_json::Value) {
        let n = normalize_spec_input(&v).expect("归一化不该失败");
        serde_json::from_value::<GameSpec>(n)
            .unwrap_or_else(|e| panic!("归一化后仍无法反序列化：{e}"));
    }

    // ── 信封（实测 21 次失败）──

    /// `{"spec": {...}}`——模型自己发明的包装。
    #[test]
    fn unwraps_spec_envelope() {
        assert_builds(serde_json::json!({"spec": minimal()}));
    }

    /// `{"arguments": {...}}`——从我们**泄漏给它的**参数面形状照抄来的。
    #[test]
    fn unwraps_arguments_envelope() {
        assert_builds(serde_json::json!({"arguments": minimal()}));
    }

    /// 信封里装的是**字符串化的** JSON（两层都套）。
    #[test]
    fn unwraps_envelope_holding_json_string() {
        let inner = serde_json::to_string(&minimal()).unwrap();
        assert_builds(serde_json::json!({"spec": inner}));
    }

    // ── 子容器字符串化（实测 21 次失败）──

    /// `payoffs` 是字符串——实测 8 次 `invalid type: string, expected a struct`。
    #[test]
    fn unwraps_stringified_payoffs() {
        let mut spec = minimal();
        spec["payoffs"] = serde_json::Value::String(
            serde_json::to_string(&serde_json::json!({
                "form": "normal", "values": {"cells": {"进,挡": {"us": 1, "them": 2}}}
            }))
            .unwrap(),
        );
        assert_builds(spec);
    }

    /// `params` 是字符串 `"{}"`——实测 8 次 `invalid type: string "{}", expected a map`。
    ///
    /// 这个尤其刺眼：**空参数表被拒了 8 次**，而它本该是最省事的形态。
    #[test]
    fn unwraps_stringified_params() {
        let mut spec = minimal();
        spec["params"] = serde_json::Value::String("{}".into());
        assert_builds(spec);
    }

    /// `strategies` 是字符串。
    #[test]
    fn unwraps_stringified_strategies() {
        let mut spec = minimal();
        spec["strategies"] =
            serde_json::Value::String(r#"{"us": ["进"], "them": ["挡"]}"#.into());
        assert_builds(spec);
    }

    /// 容器字符串化但**内容真的坏了** ⇒ 报错必须**指名是哪个键**。
    ///
    /// 裸 serde 只会说 `expected a map`，那句话没告诉模型错在哪一层。
    #[test]
    fn broken_container_string_names_the_key() {
        let mut spec = minimal();
        spec["params"] = serde_json::Value::String("{不是 JSON".into());
        let err = normalize_spec_input(&spec).expect_err("坏内容应当报错");
        assert!(err.contains("`params`"), "报错要点名出错的键：{err}");
        assert!(
            err.contains("去掉外层引号") || err.contains("容器本身"),
            "报错要说清怎么改：{err}"
        );
    }

    // ── meta（实测：为一个展示字段整份退回）──

    /// `meta.framework` 写成数组——实测 `invalid type: sequence, expected a string`。
    #[test]
    fn coerces_meta_array_value_to_string() {
        let mut spec = minimal();
        spec["meta"] = serde_json::json!({
            "framework": ["rubinstein-bargaining", "nash-bargaining"]
        });
        let n = normalize_spec_input(&spec).unwrap();
        let v = n["meta"]["framework"].as_str().expect("数组应被转成字符串");
        assert!(v.contains("rubinstein-bargaining"), "内容不得丢失：{v}");
        assert!(v.contains("nash-bargaining"), "内容不得丢失：{v}");
        assert_builds(n);
    }

    /// `meta` 里的数字/布尔也转成字符串（同为展示字段，同样不该拒绝）。
    #[test]
    fn coerces_meta_scalar_values() {
        let mut spec = minimal();
        spec["meta"] = serde_json::json!({"horizon_months": 12, "draft": false});
        let n = normalize_spec_input(&spec).unwrap();
        assert_eq!(n["meta"]["horizon_months"], "12");
        assert_eq!(n["meta"]["draft"], "false");
    }

    // ── 顶层形态错误要说清「收到了什么」──

    /// 传数组 ⇒ 报错要说明收到的是数组，而不是一句 "必须是 JSON 对象"。
    #[test]
    fn array_input_reports_actual_kind() {
        let err = normalize_spec_input(&serde_json::json!([1, 2])).expect_err("数组应被拒");
        assert!(err.contains("数组"), "要点明实际类型：{err}");
    }

    // ── 既有行为不得回归 ──

    /// 裸数字 → `Expr::Literal`（旧行为，见 `normalize_spec_input` 文档 §1）。
    #[test]
    fn bare_numbers_become_literal_expr() {
        let n = normalize_spec_input(&minimal()).unwrap();
        let cell = &n["payoffs"]["values"]["cells"]["进,挡"]["us"];
        assert_eq!(cell["kind"], "literal");
        assert_eq!(cell["value"], 1);
    }

    /// 裸字符串 → `Expr::Ref`（既有行为）。
    #[test]
    fn bare_strings_become_ref_expr() {
        let mut spec = minimal();
        spec["payoffs"]["values"]["cells"]["进,挡"]["us"] = serde_json::json!("p_cost");
        let n = normalize_spec_input(&spec).unwrap();
        assert_eq!(n["payoffs"]["values"]["cells"]["进,挡"]["us"]["kind"], "ref");
    }

    /// `payoffs` 缺 `form` 时补 `normal`（既有行为）。
    #[test]
    fn missing_form_defaults_to_normal() {
        let mut spec = minimal();
        spec["payoffs"] = serde_json::json!({
            "values": {"cells": {"进,挡": {"us": 1, "them": 2}}}
        });
        let n = normalize_spec_input(&spec).unwrap();
        assert_eq!(n["payoffs"]["form"], "normal");
    }

    #[test]
    fn deep_merge_replaces_variant_objects_without_leaking_stale_fields() {
        let base = serde_json::json!({
            "params": {
                "p_price": {
                    "name": "单杯定价",
                    "value": {"kind": "literal", "value": 40},
                    "source": {"kind": "public_data", "provenance": "财报"}
                }
            },
            "payoffs": {
                "form": "normal",
                "values": {"cells": {"进,挡": {"us": 10, "them": 5}}}
            }
        });
        let patch = serde_json::json!({
            "params": {
                "p_price": {
                    "value": {"kind": "ref", "value": "p_ref"},
                    "source": {"kind": "user_confirmed", "checkpoint": "cp2"}
                }
            },
            "payoffs": {
                "form": "extensive",
                "values": {"tree": {"root": "n0"}}
            }
        });
        let merged = super::deep_merge(base, patch);
        assert_eq!(merged["params"]["p_price"]["name"], "单杯定价");
        assert_eq!(merged["params"]["p_price"]["value"]["kind"], "ref");
        assert_eq!(merged["params"]["p_price"]["source"]["kind"], "user_confirmed");
        assert!(merged["params"]["p_price"]["source"].get("provenance").is_none());
        assert_eq!(merged["payoffs"]["form"], "extensive");
        assert!(merged["payoffs"]["values"].get("cells").is_none());
        assert_eq!(merged["payoffs"]["values"]["tree"]["root"], "n0");
    }
}

/// 「校验失败」的报错必须**给出出路**，而不只是宣布失败。
///
/// 实测依据（`case-20260919-scenario-063033`）：模型把六个拿不准的参数
/// 诚实标成 `assumed`，然后在这个错误上耗掉 61 次调用 —— 因为原报错只说
/// "不得进入求解"，**没说那该怎么办**，于是它试遍了所有写法，
/// 唯独没试唯一有效的那条（先摆给用户确认）。
#[cfg(test)]
mod validation_message_tests {
    use super::render_validation;
    use shangyi_gamespec::ValidationError;

    #[test]
    fn unconfirmed_param_message_names_the_way_out() {
        let msg = render_validation(&[ValidationError::UnconfirmedParam("p_cost".into())]);
        assert!(
            msg.contains("checkpoint") && msg.contains("cp2"),
            "必须点名 checkpoint 这条路（否则模型只会反复重发）：{msg}"
        );
        assert!(
            msg.contains("confirmed_assumption") && msg.contains("basis"),
            "必须给出通过校验所需的**确切 source 形状**：{msg}"
        );
        assert!(
            msg.contains("gamespec_build"),
            "必须说明确认后要重新构建：{msg}"
        );
    }

    /// 与之无关的校验错误**不该**带上那段话（否则是噪声，
    /// 且会误导模型去开一个不相干的 checkpoint）。
    #[test]
    fn unrelated_errors_do_not_mention_checkpoint() {
        let msg = render_validation(&[ValidationError::PlayerCount(1)]);
        assert!(
            !msg.contains("user_confirmed"),
            "非假设类错误不该引导去改 source：{msg}"
        );
    }
}

#[cfg(test)]
mod offline_replay_tests {
    use super::DomainExecutor;
    use shangyi_gamespec::GameSpec;
    use shangyi_intel::sidecar::{
        ComputationBoundary, EncodingDetail, FileInfo, HeaderInfo, IngestReport,
    };
    use shangyi_kb::TheoryLibrary;
    use shangyi_playbooks::Playbook;

    fn test_playbook() -> Playbook {
        Playbook::from_yaml(include_str!(
            "../../../playbooks/pricing_response/playbook.yml"
        ))
        .expect("builtin pricing_response playbook")
    }

    fn minimal_spec_and_exec<'a>(
        theory: &'a TheoryLibrary,
        pb: &'a Playbook,
    ) -> DomainExecutor<'a> {
        let mut ex = DomainExecutor::new(theory, pb);
        let spec: GameSpec = serde_json::from_value(serde_json::json!({
            "game_id": "offline-replay",
            "players": [
                {"id": "us", "name": "我方", "vn_role": "focal", "relation_to_game": "player"},
                {"id": "rival", "name": "对手", "vn_role": "substitutor", "relation_to_game": "player"},
                {"id": "ext", "name": "监管", "vn_role": "complementor", "relation_to_game": "external_actor"}
            ],
            "strategies": {"us": ["维持"], "rival": ["维持"]},
            "payoffs": {"form": "normal", "values": {"cells": {
                "维持,维持": {"us": {"kind": "literal", "value": 1}, "rival": {"kind": "literal", "value": 1}}
            }}},
            "params": {}
        }))
        .expect("valid test GameSpec");
        ex.state.game_spec = Some(spec);
        ex
    }

    #[test]
    fn restore_ingested_report_ignores_bad_json_and_appends_valid_reports() {
        let theory = TheoryLibrary::new();
        let pb = test_playbook();
        let mut ex = DomainExecutor::new(&theory, &pb);
        ex.restore_ingested_report("{not valid json");
        assert!(ex.state().ingested.is_empty());

        let report = IngestReport {
            file: FileInfo {
                path: "/missing/sales.csv".to_string(),
                size_bytes: 10,
                mtime_ms: 1,
                sha256: None,
                digest_note: None,
            },
            encoding: "utf-8".to_string(),
            encoding_detail: EncodingDetail {
                method: "utf8_strict".to_string(),
                candidates_tried: vec![],
                pinned: false,
                note: String::new(),
            },
            delimiter: ",".to_string(),
            delimiter_source: "sniffer".to_string(),
            columns: vec!["a".to_string()],
            columns_raw: vec!["a".to_string()],
            header: HeaderInfo {
                has_header: true,
                confidence: "high".to_string(),
                evidence: String::new(),
                first_row_raw: vec!["a".to_string()],
            },
            row_count: 3,
            row_count_exact: true,
            scanned_rows: 3,
            scan_limit: 100,
            sample_rows: Vec::new(),
            sample_rows_truncated: false,
            column_hints: Vec::new(),
            column_count: 1,
            ragged_rows: 0,
            blank_lines: 0,
            notes: Vec::new(),
            computation_boundary: ComputationBoundary::default(),
        };
        let j = serde_json::to_string(&report).unwrap();
        ex.restore_ingested_report(&j);
        ex.restore_ingested_report(&j);
        assert_eq!(ex.state().ingested.len(), 2);
        assert_eq!(ex.state().ingested[0].row_count, 3);
    }

    #[test]
    fn restore_swarm_output_registers_only_accepted_player_moves_idempotently() {
        let theory = TheoryLibrary::new();
        let pb = test_playbook();
        let mut ex = minimal_spec_and_exec(&theory, &pb);
        ex.restore_swarm_output("{invalid");
        assert!(ex.state().swarm_output.is_none());

        let payload = serde_json::json!({
            "rollouts": 1,
            "counts": {"novel_moves_registered": 1},
            "novel_move_verdicts": [
                {"actor": "us", "action": "非常规新招", "accepted": true, "rejection": []},
                {"actor": "us", "action": "被拒招数", "accepted": false, "rejection": ["缺依据"]},
                {"actor": "ext", "action": "非直接玩家动作", "accepted": true, "rejection": []},
                {"actor": "ghost", "action": "未知玩家动作", "accepted": true, "rejection": []}
            ]
        })
        .to_string();

        ex.restore_swarm_output(&payload);
        ex.restore_swarm_output(&payload);
        assert!(ex.state().swarm_output.is_some());
        let spec = ex.state().game_spec.as_ref().unwrap();
        assert_eq!(
            spec.strategies["us"],
            vec!["维持".to_string(), "[非常规] 非常规新招".to_string()]
        );
        assert!(!spec.strategies.contains_key("ext"));
        assert!(!spec.strategies.contains_key("ghost"));
    }
}
