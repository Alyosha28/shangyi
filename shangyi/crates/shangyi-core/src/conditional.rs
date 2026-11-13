//! Evidence-bounded delivery when a consultation cannot support a complete game.
//! This never fills payoffs, changes participants, approves assumptions or emits equilibria.
use crate::task_scope::{ScopedTaskResult, TaskResultStatus, TaskScope};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceGapReason {
    UnknownOpponentPayoff,
    UnknownOwnPayoff,
    UnconfirmedPayoffAssumption,
    IncomparablePayoff,
    MissingDecisionEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceGap {
    pub reason: EvidenceGapReason,
    pub participant_id: Option<String>,
    pub profile: Option<String>,
    pub detail: String,
}

/// A snapshot of actual executor state, not a model-authored completion payload.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConditionalSnapshot {
    pub question: String,
    pub attempted_model: Option<Value>,
    pub gaps: Vec<EvidenceGap>,
    pub cost_scenarios: Vec<Value>,
    pub source_urls: Vec<String>,
    pub retrieval_attempts: usize,
    pub constraint_evaluation: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionalTrigger {
    ExplicitRequest,
    RepeatedNoProgress,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionalCostComparison {
    pub baseline_scenario_id: String,
    pub alternative_scenario_id: String,
    pub baseline_scenario: String,
    pub alternative_scenario: String,
    pub currency: String,
    #[serde(default, deserialize_with = "optional_finite_number")]
    pub horizon_months: Option<f64>,
    /// Alternative minus baseline, on the same horizon; never a profit difference.
    #[serde(default, deserialize_with = "optional_finite_number")]
    pub listed_cash_expense_difference: Option<f64>,
    /// Baseline minus alternative: a difference of listed monthly cash subtotals.
    #[serde(default, deserialize_with = "optional_finite_number")]
    pub listed_monthly_cash_difference: Option<f64>,
    #[serde(default, deserialize_with = "optional_finite_number")]
    pub listed_deposit_difference: Option<f64>,
    #[serde(default, deserialize_with = "optional_finite_number")]
    pub listed_opportunity_cost_difference: Option<f64>,
    #[serde(default, deserialize_with = "optional_finite_number")]
    pub listed_incremental_upfront_cash_expense: Option<f64>,
    pub coverage_condition: String,
    /// Arithmetic threshold for the alternative's listed cash expense only.
    /// Its applicability assumptions are explicitly unconfirmed.
    #[serde(default, deserialize_with = "optional_finite_number")]
    pub listed_cost_coverage_months: Option<f64>,
    pub applicability_assumptions_confirmed: bool,
    pub limitation: String,
}

// `session::Event` is internally tagged, so under `serde_json/arbitrary_precision`
// its content is buffered through serde's `Content` and numbers arrive as a private
// map instead of a plain number. `Value` understands that representation; a direct
// f64 visitor does not. The feature now exists only in **test builds** (enabled by
// the `[dev-dependencies]` of `shangyi-gamespec` / `shangyi-intel`; before R1 it
// reached the whole graph through the upstream vendor crate). Going through `Value`
// keeps this helper correct either way, so the JSON wire does not depend on the
// build profile. Keep the wire as ordinary JSON numbers and retain null as unknown,
// never as zero.
fn optional_finite_number<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|number| number.is_finite())
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("expected a finite JSON number or null")),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionalAnalysis {
    pub trigger: ConditionalTrigger,
    pub snapshot: ConditionalSnapshot,
    pub cost_comparisons: Vec<ConditionalCostComparison>,
    pub decision_conditions: Vec<String>,
    pub information_actions: Vec<String>,
    pub limitations: Vec<String>,
}

/// Identity deliberately excludes error prose: rewording an unchanged gap is not progress.
pub fn gap_fingerprint(gaps: &[EvidenceGap]) -> String {
    let mut keys: Vec<_> = gaps
        .iter()
        .map(|g| (&g.reason, &g.participant_id, &g.profile))
        .collect();
    keys.sort();
    keys.dedup();
    serde_json::to_string(&keys).expect("gap identities are serializable")
}

fn number(value: &Value, pointer: &str) -> Option<f64> {
    value
        .pointer(pointer)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
}
fn difference(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    a.zip(b).map(|(a, b)| a - b).filter(|n| n.is_finite())
}
fn known_item(cost: &Value, treatment: &str, period: Option<&str>) -> bool {
    cost["items"].as_array().is_some_and(|items| {
        items.iter().any(|item| {
            item["treatment"] == treatment
                && period.map_or(true, |p| item["normalized_period"] == p)
                && item["normalized_amount"]
                    .as_f64()
                    .is_some_and(f64::is_finite)
        })
    })
}
fn label(cost: &Value) -> String {
    cost["scenario"]
        .as_str()
        .or_else(|| cost["scenario_id"].as_str())
        .unwrap_or("未命名方案")
        .to_string()
}
fn eligible(cost: &Value, review: Option<&Value>) -> bool {
    if cost["comparison_only"] == true || cost["constraint_assessment"]["status"] != "eligible" {
        return false;
    }
    let revision = review
        .and_then(|v| v.get("revision").or_else(|| v.get("constraint_revision")))
        .and_then(Value::as_u64);
    revision.map_or(true, |r| {
        cost["constraint_assessment"]["revision"].as_u64() == Some(r)
    })
}
fn cost_comparisons(costs: &[Value], review: Option<&Value>) -> Vec<ConditionalCostComparison> {
    let mut comparisons = Vec::new();
    for (i, baseline) in costs.iter().enumerate() {
        for alternative in costs.iter().skip(i + 1) {
            if !eligible(baseline, review) || !eligible(alternative, review) {
                continue;
            }
            // Orient only the arithmetic comparison, not a recommendation or a ranking.
            let (baseline, alternative) = if number(baseline, "/listed_monthly_fixed_cost")
                .zip(number(alternative, "/listed_monthly_fixed_cost"))
                .is_some_and(|(a, b)| a < b)
            {
                (alternative, baseline)
            } else {
                (baseline, alternative)
            };
            let Some(currency) = baseline["currency"]
                .as_str()
                .filter(|c| !c.trim().is_empty())
            else {
                continue;
            };
            if alternative["currency"].as_str() != Some(currency) {
                continue;
            }
            let baseline_monthly = known_item(baseline, "cash_expense", Some("month"))
                .then(|| number(baseline, "/listed_monthly_fixed_cost"))
                .flatten();
            let alternative_monthly = known_item(alternative, "cash_expense", Some("month"))
                .then(|| number(alternative, "/listed_monthly_fixed_cost"))
                .flatten();
            let monthly_difference = difference(baseline_monthly, alternative_monthly);
            let same_horizon = number(baseline, "/horizon_totals/months")
                .zip(number(alternative, "/horizon_totals/months"))
                .filter(|(a, b)| a == b)
                .map(|(a, _)| a);
            let expense_difference = if same_horizon.is_some()
                && known_item(baseline, "cash_expense", None)
                && known_item(alternative, "cash_expense", None)
            {
                difference(
                    number(alternative, "/horizon_totals/listed_cash_expense"),
                    number(baseline, "/horizon_totals/listed_cash_expense"),
                )
            } else {
                None
            };
            let deposit_difference = if known_item(baseline, "refundable_deposit", None)
                && known_item(alternative, "refundable_deposit", None)
            {
                difference(
                    number(alternative, "/listed_refundable_deposits"),
                    number(baseline, "/listed_refundable_deposits"),
                )
            } else {
                None
            };
            let opportunity_difference = if same_horizon.is_some()
                && known_item(baseline, "opportunity_cost", None)
                && known_item(alternative, "opportunity_cost", None)
            {
                difference(
                    number(alternative, "/horizon_totals/listed_opportunity_cost"),
                    number(baseline, "/horizon_totals/listed_opportunity_cost"),
                )
            } else {
                None
            };
            let incremental = if known_item(baseline, "cash_expense", Some("once"))
                && known_item(alternative, "cash_expense", Some("once"))
            {
                difference(
                    number(alternative, "/listed_upfront_cash_expense"),
                    number(baseline, "/listed_upfront_cash_expense"),
                )
            } else {
                None
            };
            let coverage = monthly_difference
                .filter(|n| *n > 0.0)
                .zip(incremental)
                .map(|(saving, expense)| expense.max(0.0) / saving)
                .filter(|n| n.is_finite() && *n >= 0.0);
            let coverage_condition=match (monthly_difference,incremental) {
                (Some(monthly),Some(once)) if monthly>0.0=>format!("在同口径下，持续月数 ×（已列月费用差额 {monthly} + 未列月费用差额）≥ 已列一次性现金费用增量 {once} + 未列一次性费用差额。未列部分保持变量，不能默认为零。"),
                (Some(monthly),_) if monthly>0.0=>format!("持续月数 ×（已列月费用差额 {monthly} + 未列月费用差额）须覆盖两方案的一次性现金费用增量；至少一方的一次性费用尚未明确列出，不能当零，暂不提供数字覆盖月份。"),
                _=>"目前没有可使用的正向已列月费用差额，不能据此给出覆盖月份；仍需比较两方案同口径的费用和未知收益。".into(),
            };
            if monthly_difference.is_none() && expense_difference.is_none() {
                continue;
            }
            comparisons.push(ConditionalCostComparison {
                baseline_scenario_id:baseline["scenario_id"].as_str().unwrap_or_default().to_string(),
                alternative_scenario_id:alternative["scenario_id"].as_str().unwrap_or_default().to_string(),
                baseline_scenario:label(baseline),alternative_scenario:label(alternative),currency:currency.to_string(),
                horizon_months:same_horizon,listed_cash_expense_difference:expense_difference,
                listed_monthly_cash_difference:monthly_difference,listed_deposit_difference:deposit_difference,
                listed_opportunity_cost_difference:opportunity_difference,listed_incremental_upfront_cash_expense:incremental,
                coverage_condition,listed_cost_coverage_months:coverage,
                applicability_assumptions_confirmed:false,
                limitation:"只比较已列现金费用；未列项目不是零，也可能改变差额。覆盖门槛仅在月费用固定、列项口径可比且遗漏费用不改变差额时适用，这些前提仍待核对。押金占用及退款时点、机会损失另列；不表示净利润、实际现金轨迹或对方会接受。".into(),
            });
        }
    }
    comparisons
}

pub fn validate_conditional_result(result: &ScopedTaskResult) -> Result<(), String> {
    if result.scope != TaskScope::Conditional || result.status != TaskResultStatus::Partial {
        return Err("条件分析必须显式标为 conditional/partial，不能冒充完整咨询完成".into());
    }
    let analysis = result
        .conditional
        .as_ref()
        .ok_or("条件分析缺少可审计的原始快照")?;
    if analysis.snapshot.gaps.is_empty()
        || result.missing.is_empty()
        || result.summary.trim().is_empty()
    {
        return Err("条件分析必须保留真实的类型化缺口及交付正文".into());
    }
    if analysis
        .cost_comparisons
        .iter()
        .any(|c| c.applicability_assumptions_confirmed)
    {
        return Err("条件交付不能替用户确认比较前提".into());
    }
    Ok(())
}

pub fn build_conditional_result(
    snapshot: &ConditionalSnapshot,
    trigger: ConditionalTrigger,
) -> Result<ScopedTaskResult, String> {
    if snapshot.question.trim().is_empty() {
        return Err("条件分析须保留用户正在判断的问题".into());
    }
    if snapshot.gaps.is_empty() || snapshot.gaps.iter().any(|g| g.detail.trim().is_empty()) {
        return Err("条件分析须有明确的类型化证据缺口，不能绕过完整求解".into());
    }
    let comparisons = cost_comparisons(
        &snapshot.cost_scenarios,
        snapshot.constraint_evaluation.as_ref(),
    );
    let mut conditions = Vec::new();
    let mut actions = Vec::new();
    for gap in &snapshot.gaps {
        let named = gap.participant_id.as_deref().and_then(|id| {
            snapshot
                .attempted_model
                .as_ref()
                .and_then(|m| m.get("players"))
                .and_then(Value::as_array)
                .and_then(|ps| ps.iter().find(|p| p["id"].as_str() == Some(id)))
                .and_then(|p| p["name"].as_str())
                .filter(|name| !name.trim().is_empty())
        });
        let party = named.unwrap_or(if gap.reason == EvidenceGapReason::UnknownOwnPayoff {
            "你这边"
        } else {
            "对方"
        });
        match gap.reason {
            EvidenceGapReason::UnknownOpponentPayoff|EvidenceGapReason::UnknownOwnPayoff => {
                conditions.push(format!("从{party}的角度，只有在同一期限、同一收益口径下，本方案整体收益不低于其替代选择时，才有相应的收益激励。当前收益仍未知，不能判断此条件成立，也不能从租金收入直接推出净利润或实际接受意愿。"));
                actions.push(format!("核实{party}在本方案与替代方案下的收入、成本、可用外部选择及期间；能取得区间就保留区间，拿不到则继续保留为变量，不填 0。"));
            }
            EvidenceGapReason::UnconfirmedPayoffAssumption => actions.push("把待用假设、数值范围与它会改变的结论逐项展示给用户；只有实际确认后才能用于依赖该假设的求解，不把情景假设升级为事实。".into()),
            EvidenceGapReason::IncomparablePayoff => actions.push("先统一双方选择的期间、收入/成本/净收益口径，避免把两年收入和几个月机会损失直接相减；保留押金、现金费用和机会损失的区别。".into()),
            EvidenceGapReason::MissingDecisionEvidence => actions.push(format!("补齐会改变这项决策的具体信息：{}；能够公开核对的由检索取得，只有内部目标与底线才向用户确认。",gap.detail)),
        }
    }
    if comparisons.is_empty() {
        actions.push("若要比较自己的支出，提供至少两个同币种、同期间方案的租金及一次性现金费用；押金和停业少赚分别列出，再由费用工具计算。不凭缺项推断哪一方案更便宜。".into());
    }
    conditions.sort();
    conditions.dedup();
    actions.sort();
    actions.dedup();
    let limitations=vec![
        "这是一份保留未知条件的咨询交付，不是均衡解、接受概率或利润预测；完整求解的校验与用户确认门槛保持不变。".into(),
        "原参与者、策略和已取得依据保持原样；不能删除对手、改成背景方或用零占位未知收益以通过校验。".into(),
        "用户已给出的硬约束仍生效；条件比较不会使超出约束的行动变成可推荐选项，约束状态未知时须补充核对。".into(),
    ];
    let analysis = ConditionalAnalysis {
        trigger,
        snapshot: snapshot.clone(),
        cost_comparisons: comparisons,
        decision_conditions: conditions,
        information_actions: actions,
        limitations,
    };
    let mut summary = format!(
        "## 先给你能用的条件判断\n\n你正在考虑：{}\n\n完整比较所需的关键依据尚未取得或核对。我保留这些未知，先交付可核对的费用比较、会改变决定的条件和下一步信息；不替任何一方填零收益。\n",
        snapshot.question
    );
    summary.push_str("\n## 已取得的费用与可以核对的差额\n");
    if snapshot.cost_scenarios.is_empty() {
        summary.push_str("\n目前没有可引用的成功费用计算结果，因此这里不生成费用数字。\n");
    }
    for cost in &snapshot.cost_scenarios {
        let acceptance = if eligible(cost, snapshot.constraint_evaluation.as_ref()) {
            "已满足当前已登记限制，收益仍待核对"
        } else if cost["constraint_assessment"]["status"] == "ineligible" {
            "超出已登记限制，仅作报价对照，不是可推荐选项"
        } else {
            "约束状态尚未核对，不能列为已可接受选项"
        };
        summary.push_str(&format!("\n- **{}**（{acceptance}）：", label(cost)));
        let mut known = Vec::new();
        for (field, treatment, period, title) in [
            (
                "/listed_upfront_cash_expense",
                "cash_expense",
                Some("once"),
                "已列一次性现金费用",
            ),
            (
                "/listed_monthly_fixed_cost",
                "cash_expense",
                Some("month"),
                "已列月现金费用",
            ),
            (
                "/listed_refundable_deposits",
                "refundable_deposit",
                None,
                "已列可退押金占用",
            ),
            (
                "/listed_upfront_opportunity_cost",
                "opportunity_cost",
                Some("once"),
                "已列一次性机会损失（不扣现金）",
            ),
        ] {
            if known_item(cost, treatment, period) {
                if let Some(value) = number(cost, field) {
                    known.push(format!(
                        "{title} {value} {}",
                        cost["currency"].as_str().unwrap_or("未注明币种")
                    ));
                }
            }
        }
        summary.push_str(&if known.is_empty() {
            "该计算没有可引用的已知金额，未知项仍保留。".into()
        } else {
            known.join("；")
        });
        summary.push_str("。这里只引用工具已列项目，未列费用不视为零。\n");
    }
    for comparison in &analysis.cost_comparisons {
        summary.push_str(&format!(
            "\n### {} 与 {}\n",
            comparison.baseline_scenario, comparison.alternative_scenario
        ));
        if let (Some(months), Some(value)) = (
            comparison.horizon_months,
            comparison.listed_cash_expense_difference,
        ) {
            summary.push_str(&format!("\n按相同的 {months} 个月，后者减前者的已列现金费用小计为 {value} {}；这不是利润差额。\n",comparison.currency));
        }
        if let Some(value) = comparison.listed_monthly_cash_difference {
            summary.push_str(&format!(
                "\n前者减后者的已列月现金费用小计为 {value} {}。\n",
                comparison.currency
            ));
        }
        summary.push_str(&format!("\n{}\n", comparison.coverage_condition));
        if let Some(months) = comparison.listed_cost_coverage_months {
            summary.push_str(&format!("\n仅按双方已明确列出的现金费用增量，算术门槛为 {months:.2} 个月（后者减前者的一次性现金费用增量 ÷ 月小计差额，负增量按无需覆盖处理）。**这不是已确认的回本期**；未列费用差额仍是变量，只有比较前提经核对后才可用于决定。\n"));
        }
        summary.push_str(&format!("\n{}\n", comparison.limitation));
    }
    summary.push_str("\n## 哪些条件会改变决定\n");
    for condition in &analysis.decision_conditions {
        summary.push_str(&format!("\n- {condition}"));
    }
    if analysis.decision_conditions.is_empty() {
        summary
            .push_str("\n先满足原有硬约束并补齐下列关键缺口；当前信息不足以判断完整方案的优劣。\n");
    }
    summary.push_str("\n\n## 具体补什么信息\n");
    for action in &analysis.information_actions {
        summary.push_str(&format!("\n- {action}"));
    }
    summary.push_str("\n\n## 保留的关键缺口\n");
    let mut missing: Vec<String> = snapshot.gaps.iter().map(|g| g.detail.clone()).collect();
    for cost in &snapshot.cost_scenarios {
        if cost["costs_complete"] != true {
            missing.push(format!(
                "方案「{}」仍有未列或未取得费用，现有差额不代表完整成本。",
                label(cost)
            ));
        }
    }
    missing.sort();
    missing.dedup();
    for gap in &missing {
        summary.push_str(&format!("\n- {gap}"));
    }
    summary.push_str("\n\n");
    summary.push_str(&analysis.limitations.join("\n\n"));
    let result = ScopedTaskResult {
        scope: TaskScope::Conditional,
        status: TaskResultStatus::Partial,
        summary,
        missing,
        source_urls: snapshot.source_urls.clone(),
        retrieval_attempts: snapshot.retrieval_attempts,
        cost_scenarios: snapshot.cost_scenarios.clone(),
        conditional: Some(analysis),
        research: None,
    };
    validate_conditional_result(&result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn gap() -> EvidenceGap {
        EvidenceGap {
            reason: EvidenceGapReason::UnknownOpponentPayoff,
            participant_id: Some("landlord".into()),
            profile: Some("stay,accept".into()),
            detail: "房东在外租选择下的完整收益未知".into(),
        }
    }
    fn snapshot() -> ConditionalSnapshot {
        ConditionalSnapshot {
            question: "在租金底线内怎样谈续租或搬店？".into(),
            attempted_model: Some(
                json!({"players":[{"id":"tenant","relation_to_game":"player"},{"id":"landlord","relation_to_game":"player"}],"strategies":{"tenant":["stay","move"],"landlord":["accept","reject"]},"payoffs":{"unknown":"kept"}}),
            ),
            gaps: vec![gap()],
            ..Default::default()
        }
    }
    fn cost(id: &str, rent: f64, once: f64, deposit: f64) -> Value {
        json!({"scenario_id":id,"scenario":id,"currency":"CNY","constraint_assessment":{"status":"eligible","revision":0},"costs_complete":false,"listed_monthly_fixed_cost":rent,"listed_upfront_cash_expense":once,"listed_refundable_deposits":deposit,"listed_upfront_opportunity_cost":20000,"horizon_totals":{"months":24,"listed_cash_expense":once+rent*24.0},"items":[{"treatment":"cash_expense","normalized_period":"month","normalized_amount":rent},{"treatment":"cash_expense","normalized_period":"once","normalized_amount":once},{"treatment":"refundable_deposit","normalized_period":"once","normalized_amount":deposit},{"treatment":"opportunity_cost","normalized_period":"once","normalized_amount":20000}]})
    }
    #[test]
    fn compares_only_recorded_cash_costs_without_using_deposits_or_opportunity_loss_as_fees() {
        let mut input = snapshot();
        input.cost_scenarios = vec![
            cost("stay", 11000.0, 0.0, 0.0),
            cost("move", 9000.0, 60000.0, 18000.0),
        ];
        let original = input.clone();
        let result = build_conditional_result(&input, ConditionalTrigger::ExplicitRequest).unwrap();
        assert_eq!(input, original);
        assert_eq!(result.scope, TaskScope::Conditional);
        assert!(!result.is_complete());
        let report = result.conditional.unwrap();
        assert_eq!(report.snapshot.attempted_model, original.attempted_model);
        let cmp = &report.cost_comparisons[0];
        assert_eq!(cmp.listed_cash_expense_difference, Some(12000.0));
        assert_eq!(cmp.listed_cost_coverage_months, Some(30.0));
        assert_eq!(cmp.listed_deposit_difference, Some(18000.0));
        assert!(!cmp.applicability_assumptions_confirmed);
        assert!(result.summary.contains("不是已确认的回本期"));
    }
    #[test]
    fn unknown_cost_subtotal_zero_is_not_an_observed_zero_or_numeric_threshold() {
        let mut input = snapshot();
        let mut unknown = cost("unknown", 0.0, 0.0, 0.0);
        unknown["items"] = json!([{"treatment":"cash_expense","normalized_period":"month","normalized_amount":null}]);
        input.cost_scenarios = vec![unknown, cost("move", 9000.0, 60000.0, 18000.0)];
        let result =
            build_conditional_result(&input, ConditionalTrigger::RepeatedNoProgress).unwrap();
        assert!(
            result
                .conditional
                .as_ref()
                .unwrap()
                .cost_comparisons
                .is_empty()
        );
        assert!(
            result.summary.contains("不生成费用数字")
                || result.summary.contains("没有可引用的已知金额")
        );
    }
    #[test]
    fn incomparable_currency_or_horizon_is_never_subtracted() {
        let mut input = snapshot();
        let a = cost("a", 11000.0, 0.0, 0.0);
        let mut b = cost("b", 9000.0, 60000.0, 18000.0);
        b["currency"] = json!("USD");
        input.cost_scenarios = vec![a.clone(), b.clone()];
        assert!(
            build_conditional_result(&input, ConditionalTrigger::ExplicitRequest)
                .unwrap()
                .conditional
                .unwrap()
                .cost_comparisons
                .is_empty()
        );
        b["currency"] = json!("CNY");
        b["horizon_totals"]["months"] = json!(36);
        input.cost_scenarios = vec![a, b];
        assert!(
            build_conditional_result(&input, ConditionalTrigger::ExplicitRequest)
                .unwrap()
                .conditional
                .unwrap()
                .cost_comparisons[0]
                .listed_cash_expense_difference
                .is_none()
        );
    }
    #[test]
    fn rephrasing_a_gap_is_not_evidence_progress_and_no_gap_cannot_bypass_solving() {
        let a = gap();
        let mut b = a.clone();
        b.detail = "换个说法，对方收益仍没取得".into();
        assert_eq!(gap_fingerprint(&[a]), gap_fingerprint(&[b]));
        let mut input = snapshot();
        input.gaps.clear();
        assert!(build_conditional_result(&input, ConditionalTrigger::ExplicitRequest).is_err());
    }
    #[test]
    fn coverage_uses_known_increment_and_never_treats_an_unlisted_baseline_fee_as_zero() {
        let mut input = snapshot();
        input.cost_scenarios = vec![
            cost("move", 9000.0, 60000.0, 18000.0),
            cost("stay", 11000.0, 20000.0, 0.0),
        ];
        let result = build_conditional_result(&input, ConditionalTrigger::ExplicitRequest).unwrap();
        assert_eq!(
            result.conditional.unwrap().cost_comparisons[0].listed_cost_coverage_months,
            Some(20.0)
        );
        input.cost_scenarios[1]["items"]
            .as_array_mut()
            .unwrap()
            .retain(|i| i["normalized_period"] != "once" || i["treatment"] != "cash_expense");
        let result = build_conditional_result(&input, ConditionalTrigger::ExplicitRequest).unwrap();
        assert!(
            result.conditional.as_ref().unwrap().cost_comparisons[0]
                .listed_cost_coverage_months
                .is_none()
        );
        assert!(result.summary.contains("不能当零"));
        assert!(!result.summary.contains("对landlord"));
    }
    #[test]
    fn ineligible_unknown_or_stale_constraint_scenarios_are_not_comparison_candidates() {
        for status in ["ineligible", "unknown"] {
            let mut input = snapshot();
            let mut b = cost("quote", 12000.0, 0.0, 0.0);
            b["constraint_assessment"]["status"] = json!(status);
            input.cost_scenarios = vec![cost("stay", 11000.0, 0.0, 0.0), b];
            assert!(
                build_conditional_result(&input, ConditionalTrigger::ExplicitRequest)
                    .unwrap()
                    .conditional
                    .unwrap()
                    .cost_comparisons
                    .is_empty()
            );
        }
        let mut input = snapshot();
        input.cost_scenarios = vec![cost("a", 11000.0, 0.0, 0.0), cost("b", 9000.0, 100.0, 0.0)];
        input.constraint_evaluation = Some(json!({"revision":1}));
        assert!(
            build_conditional_result(&input, ConditionalTrigger::ExplicitRequest)
                .unwrap()
                .conditional
                .unwrap()
                .cost_comparisons
                .is_empty()
        );
    }

    #[test]
    fn numeric_conditional_delivery_round_trips_through_the_real_case_event() {
        let mut input = snapshot();
        input.cost_scenarios = vec![
            cost("stay", 11000.0, 0.0, 0.0),
            cost("move", 9000.0, 60000.0, 18000.0),
        ];
        let event = crate::session::Event::ScopedTaskCompleted {
            result: build_conditional_result(&input, ConditionalTrigger::ExplicitRequest).unwrap(),
            at: "2026-10-02T12:00:00Z".into(),
        };
        let encoded = serde_json::to_string(&event).unwrap();
        let decoded: crate::session::Event = serde_json::from_str(&encoded)
            .expect("persisted numeric conditional result must be readable");
        assert_eq!(decoded, event);
    }
}
