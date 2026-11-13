//! Swarm 编排器（设计文档 §5.7）。
//!
//! # 编排器的职责边界
//!
//! 纯代码：**回合调度、终止条件、多样性采样**。
//! persona 的**行动**由外部（LLM 或规则）产生，经 [`PersonaActor`] trait 注入。
//!
//! # 铁律 1 的执行点
//!
//! [`evaluate_candidates`] 是"提议 → 结论"的唯一通道：
//! persona 的 [`Move`] 在此被**引擎评估**（矩阵外候选过三重过滤），
//! 通过者的后果由解析引擎算出。**persona 不产出任何最终数字。**
//!
//! # 多样性采样（§5.7 的"persona 参数扰动 × N 次 rollout"）
//!
//! 同一位 persona 的决策风格有不确定性，单次 rollout 的落点不能代表其行为分布。
//! 故编排器对 persona 参数施加**种子化扰动**并跑 N 次，
//! 以落点频率作为行为证据。**种子必须持久化**（进 [`SwarmDescriptor`]），
//! 否则崩溃恢复后重跑会得到不同分布。

use std::collections::BTreeMap;

use shangyi_consequence::{NovelMoveProposal, TripleFilter, filter_and_register};
use shangyi_gamespec::GameSpec;

use crate::{
    Action, ActionDistribution, CandidateResult, Move, PersonaSet, StopReason, SwarmError,
    Trajectory, tally_actions, validate_actors,
};

/// persona 行动提供者——由外部实现（LLM 驱动的实现或测试用的规则实现）。
///
/// **为何是 trait 而非直接调 LLM**：本 crate 不依赖任何 LLM SDK，
/// 使"铁律 1（LLM 不算数）"在架构上可强制——persona 只能*提议*，
/// 评估必经 [`evaluate_candidates`]。
pub trait PersonaActor {
    /// 给定当前博弈状态与历史，产出一个提议。
    ///
    /// `perturbation` 为本次 rollout 的 persona 参数扰动（多样性采样），
    /// 实现可据此调整决策风格（如更激进/更保守）。
    fn act(
        &self,
        _spec: &GameSpec,
        participant_id: &str,
        history: &[Move],
        perturbation: f64,
    ) -> Option<Move>;
}

/// 确定性伪随机数发生器（与 `shangyi-consequence` 的 LCG 同参数）。
///
/// 自带而非用 `rand`：可复现性要求"同种子同序列"，而 `rand` 的实现细节
/// 可能随版本变化。此处只用于抽样，不需密码学强度。
#[derive(Debug, Clone)]
pub struct SwarmRng {
    state: u64,
}

impl SwarmRng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0x2545_F491_4F6C_DD1D } else { seed },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }

    /// `[0, 1)`。
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// `[-1, 1)` 的对称扰动。
    pub fn next_perturbation(&mut self) -> f64 {
        self.next_f64() * 2.0 - 1.0
    }
}

/// 编排器配置。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrchestratorConfig {
    /// 单次 rollout 的最大回合数（终止条件之一）。
    pub max_rounds: u32,
    /// 是否施加 persona 参数扰动（多样性采样）。
    pub perturb: bool,
    /// 连续多少回合同一组行动即视为稳定（终止条件之二）。
    pub stable_rounds: u32,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            // 设计文档 §11.1：MVP 为单轮 rollout；此处默认给稍宽余量，
            // 使"稳定"与"回合上限"两个终止条件都能被触发与测试。
            max_rounds: 40,
            perturb: true,
            stable_rounds: 3,
        }
    }
}

/// 跑一次 rollout，返回轨迹。
///
/// # 终止条件（三个，取先到者）
///
/// 1. 达到 `max_rounds`
/// 2. 连续 `stable_rounds` 回合行动组合未变（事实稳定点）
/// 3. actor 无提议可给（`PersonaActor::act` 返回 `None`）
pub fn rollout(
    spec: &GameSpec,
    personas: &PersonaSet,
    actor: &dyn PersonaActor,
    config: &OrchestratorConfig,
    seed: u64,
) -> Result<Trajectory, SwarmError> {
    if personas.personas.is_empty() {
        return Err(SwarmError::EmptyPersonas);
    }
    personas.check_depth()?;

    let mut rng = SwarmRng::new(seed);
    let mut moves: Vec<Move> = Vec::new();
    // 每回合的行动签名（用于稳定性判定）
    let mut last_signature: Option<Vec<(String, String)>> = None;
    let mut stable_count = 0u32;
    let mut stop = StopReason::MaxRounds;

    for _round in 0..config.max_rounds {
        let mut round_moves: Vec<Move> = Vec::new();
        for p in &personas.personas {
            let perturbation = if config.perturb {
                rng.next_perturbation()
            } else {
                0.0
            };
            if let Some(m) = actor.act(spec, &p.participant_id, &moves, perturbation) {
                round_moves.push(m);
            }
        }

        if round_moves.is_empty() {
            stop = StopReason::Stable;
            break;
        }

        // actor 合法性：persona 幻觉出的参与者在此被拦住，
        // 否则其 move 会被静默丢弃而致分布失真
        validate_actors(spec, &round_moves)?;

        // 稳定性判定：本轮与上轮的行动签名相同 ⇒ 计入稳定计数
        let mut signature: Vec<(String, String)> = round_moves
            .iter()
            .map(|m| {
                let key = match &m.action {
                    Action::InMatrix { strategy } => strategy.clone(),
                    Action::Novel { description, .. } => description.clone(),
                };
                (m.actor.clone(), key)
            })
            .collect();
        signature.sort();

        if Some(&signature) == last_signature.as_ref() {
            stable_count += 1;
            if stable_count >= config.stable_rounds {
                moves.extend(round_moves);
                stop = StopReason::Stable;
                break;
            }
        } else {
            stable_count = 0;
        }
        last_signature = Some(signature);

        moves.extend(round_moves);
    }

    Ok(Trajectory {
        moves,
        stop_reason: stop,
        seed,
    })
}

/// 跑 N 次 rollout（多样性采样）。
pub fn rollout_n(
    spec: &GameSpec,
    personas: &PersonaSet,
    actor: &dyn PersonaActor,
    config: &OrchestratorConfig,
    n: u32,
    base_seed: u64,
) -> Result<Vec<Trajectory>, SwarmError> {
    if n == 0 {
        return Err(SwarmError::ZeroRollouts);
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        // 每次 rollout 用不同种子（确定性派生），使恢复后重跑得到同一组轨迹
        out.push(rollout(spec, personas, actor, config, base_seed.wrapping_add(i as u64))?);
    }
    Ok(out)
}

/// **引擎评估**——本 crate 的核心职责边界。
///
/// # 做什么
///
/// 1. 汇总全部轨迹的行动计数（[`tally_actions`]）
/// 2. 对**矩阵外**候选执行三重过滤（`shangyi-consequence` 的 `TripleFilter`）
/// 3. 产出的 [`CandidateResult`] 中，`engine_accepted` 由**引擎**决定，
///    而非由 persona 自报
///
/// # 为什么必须有这一步
///
/// §5.7 的铁律：**sub-agent 可以提议行动（含矩阵外），但评估一律回主引擎**。
/// 若允许 persona 自称"我这个反击有效"，那 Swarm 就变成了"LLM 算数"。
/// 本函数是这条铁律的**唯一执行点**——`CandidateResult` 没有其他公开构造路径。
///
/// # 矩阵外候选的处理
///
/// 需要从 [`Move`] 还原出 [`NovelMoveProposal`] 才能过过滤。若 `Move` 未带
/// 足够依据（合法性/可得性/收益成本），过滤会如实拒绝它——
/// 这与"无依据的非常规行动不得进博弈"（§5.7.1）一致。
// ⚠️ `spec` 目前未被使用——**这是如实标注的未完成，不是疏忽**。
//
// 它的用途在文档里已写明："需要从 `Move` 还原出 `NovelMoveProposal`"——
// 即把 persona 的提议映射回博弈的策略空间。那需要 `GameSpec` 的
// `strategies` 与 `players` 来定位，是**下一步**要实现的。
//
// 现在不改成 `_spec` 的理由：它是 `pub` API 的参数名，改名会让签名
// 看起来像"这个参数本来就不需要"——而事实是"还没接上"。
// 两者对读者与下游的意义完全不同。
#[allow(unused_variables)]
pub fn evaluate_candidates(
    spec: &GameSpec,
    trajectories: &[Trajectory],
    filter: &TripleFilter,
) -> Result<ActionDistribution, SwarmError> {
    if trajectories.is_empty() {
        return Err(SwarmError::ZeroRollouts);
    }

    let tallies = tally_actions(trajectories);
    let total_moves: u32 = tallies.values().sum();
    if total_moves == 0 {
        return Err(SwarmError::NotConverged(
            "全部 rollout 未产生任何行动".to_string(),
        ));
    }

    // 从轨迹中收集矩阵外候选的完整信息（同一描述的候选取首次出现者）
    let mut novel_specs: BTreeMap<(String, String), Move> = BTreeMap::new();
    for t in trajectories {
        for m in &t.moves {
            if let Action::Novel { description, .. } = &m.action {
                novel_specs
                    .entry((m.actor.clone(), description.clone()))
                    .or_insert_with(|| m.clone());
            }
        }
    }

    let mut entries = Vec::new();
    for ((actor, action_key, is_novel), count) in &tallies {
        let probability = *count as f64 / total_moves as f64;
        let mut engine_accepted = true;
        let mut rejection: Vec<String> = Vec::new();

        if *is_novel {
            // 矩阵外候选：**必须**过三重过滤才被引擎接受
            let Some(m) = novel_specs.get(&(actor.clone(), action_key.clone())) else {
                engine_accepted = false;
                rejection.push("矩阵外候选缺少完整信息，无法评估".to_string());
                entries.push(CandidateResult {
                    actor: actor.clone(),
                    action: action_key.clone(),
                    is_novel: *is_novel,
                    probability,
                    count: *count,
                    engine_accepted,
                    rejection,
                });
                continue;
            };
            match &m.action {
                Action::Novel {
                    description,
                    introduces,
                    legality_basis,
                    feasibility_basis,
                    expected_benefit,
                    expected_cost,
                } => {
                    let proposal = NovelMoveProposal {
                        actor_id: actor.clone(),
                        description: description.clone(),
                        targets: None,
                        introduces_actors: introduces
                            .iter()
                            .map(|id| shangyi_gamespec::Participant {
                                id: id.clone(),
                                name: id.clone(),
                                vn_role: shangyi_gamespec::ValueNetRole::Complementor,
                                relation_to_game:
                                    shangyi_gamespec::RelationToGame::ExternalActor,
                                profile_ref: None,
                            })
                            .collect(),
                        payoff_channel: "由 persona 提议，经引擎过滤".to_string(),
                        expected_benefit: shangyi_consequence::Interval::new(
                            expected_benefit.0,
                            expected_benefit.1,
                        )
                        .unwrap_or_else(|_| {
                            shangyi_consequence::Interval::new(0.0, 0.0).expect("零区间合法")
                        }),
                        expected_cost: shangyi_consequence::Interval::new(
                            expected_cost.0,
                            expected_cost.1,
                        )
                        .unwrap_or_else(|_| {
                            shangyi_consequence::Interval::new(0.0, 0.0).expect("零区间合法")
                        }),
                        legality_basis: legality_basis.clone(),
                        feasibility_basis: feasibility_basis.clone(),
                    };
                    let verdict = filter.evaluate(&proposal);
                    engine_accepted = verdict.passes();
                    rejection = verdict.rejections.clone();
                }
                Action::InMatrix { .. } => {
                    // 逻辑上不可达（is_novel 为 true）
                    engine_accepted = false;
                    rejection.push("内部不一致：标记为矩阵外但行动为矩阵内".to_string());
                }
            }
        }

        entries.push(CandidateResult {
            actor: actor.clone(),
            action: action_key.clone(),
            is_novel: *is_novel,
            probability,
            count: *count,
            engine_accepted,
            rejection,
        });
    }

    // 确定性排序：(actor, action) 升序
    entries.sort_by(|a, b| a.action_key().cmp(&b.action_key()));

    Ok(ActionDistribution {
        entries,
        rollouts: trajectories.len() as u32,
        perturbed: trajectories.iter().any(|t| t.seed != 0),
    })
}

/// 把**已通过引擎评估**的矩阵外候选注册回 `GameSpec`（§5.7.1 步骤 3）。
///
/// 只接受 `engine_accepted == true` 的候选——未通过者在类型上就不被传入。
/// 这使"未过滤的候选被误注册"不可能发生。
pub fn register_accepted_novel_moves(
    spec: &mut GameSpec,
    distribution: &ActionDistribution,
    trajectories: &[Trajectory],
    filter: &TripleFilter,
) -> usize {
    // 从轨迹还原候选
    let mut by_key: BTreeMap<(String, String), Move> = BTreeMap::new();
    for t in trajectories {
        for m in &t.moves {
            if let Action::Novel { description, .. } = &m.action {
                by_key
                    .entry((m.actor.clone(), description.clone()))
                    .or_insert_with(|| m.clone());
            }
        }
    }

    let mut proposals: Vec<NovelMoveProposal> = Vec::new();
    for e in &distribution.entries {
        if !e.is_novel || !e.engine_accepted {
            continue;
        }
        let Some(m) = by_key.get(&(e.actor.clone(), e.action.clone())) else {
            continue;
        };
        if let Action::Novel {
            description,
            introduces,
            legality_basis,
            feasibility_basis,
            expected_benefit,
            expected_cost,
        } = &m.action
        {
            proposals.push(NovelMoveProposal {
                actor_id: e.actor.clone(),
                description: description.clone(),
                targets: None,
                introduces_actors: introduces
                    .iter()
                    .map(|id| shangyi_gamespec::Participant {
                        id: id.clone(),
                        name: id.clone(),
                        vn_role: shangyi_gamespec::ValueNetRole::Complementor,
                        relation_to_game: shangyi_gamespec::RelationToGame::ExternalActor,
                        profile_ref: None,
                    })
                    .collect(),
                payoff_channel: "由 persona 提议，经引擎过滤后注册".to_string(),
                expected_benefit: shangyi_consequence::Interval::new(
                    expected_benefit.0,
                    expected_benefit.1,
                )
                .unwrap_or_else(|_| {
                    shangyi_consequence::Interval::new(0.0, 0.0).expect("零区间合法")
                }),
                expected_cost: shangyi_consequence::Interval::new(
                    expected_cost.0,
                    expected_cost.1,
                )
                .unwrap_or_else(|_| {
                    shangyi_consequence::Interval::new(0.0, 0.0).expect("零区间合法")
                }),
                legality_basis: legality_basis.clone(),
                feasibility_basis: feasibility_basis.clone(),
            });
        }
    }

    let outcome = filter_and_register(spec, &proposals, filter);
    outcome.accepted.len()
}
