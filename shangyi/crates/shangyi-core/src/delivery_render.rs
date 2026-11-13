//! Ordinary-language rendering of actual business calculation receipts.
use serde_json::Value;

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}
fn amount(value: &Value) -> String {
    value.as_f64().filter(|v| v.is_finite())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "金额未取得".into())
}
fn known_category(cost: &Value, treatment: &str, once: bool) -> bool {
    cost["items"].as_array().is_some_and(|items| items.iter().any(|item| {
        let period = item["normalized_period"].as_str().or_else(|| item["original"]["period"].as_str());
        item["treatment"] == treatment && (period == Some("once")) == once
            && item["normalized_amount"].as_f64().is_some_and(f64::is_finite)
    }))
}
pub fn render_cost_receipts(costs: &[Value]) -> String {
    let mut text = String::new();
    for cost in costs {
        let name = cell(cost["scenario"].as_str().unwrap_or("已列费用方案"));
        let currency = cell(match cost["currency"].as_str() {
            Some("CNY") => "元（人民币）", Some("USD") => "美元", Some("HKD") => "港元",
            Some("EUR") => "欧元", Some(value) => value, None => "币种未注明",
        });
        text.push_str(&format!("\n\n## {name}：已列费用\n\n金额单位：{currency}。\n\n| 项目 | 已知金额 | 口径 |\n|---|---:|---|\n"));
        for (label,key,kind,once) in [
            ("一次性现金费用","listed_upfront_cash_expense","cash_expense",true),
            ("可退押金","listed_refundable_deposits","refundable_deposit",true),
            ("每月现金费用","listed_monthly_fixed_cost","cash_expense",false),
            ("一次性机会损失","listed_upfront_opportunity_cost","opportunity_cost",true),
        ] {
            if known_category(cost,kind,once) {
                text.push_str(&format!("| {label} | {} | {} |\n",amount(&cost[key]),if kind=="opportunity_cost" {"不扣现金，另列"} else if kind=="refundable_deposit" {"占用现金，退款时间未推定"} else {"实际现金费用的已列部分"}));
            }
        }
        if cost["listed_upfront_cash"].is_number() {
            text.push_str(&format!("| 已列前期现金支出及押金 | {} | 不包含尚未列出的项目或未知付款条件 |\n",amount(&cost["listed_upfront_cash"])));
        }
        if let Some(horizon)=cost.get("horizon_totals") {
            text.push_str(&format!("| {} 个月现金费用累计 | {} | 按所填月费用固定计算，只有已列部分 |\n",amount(&horizon["months"]),amount(&horizon["listed_cash_expense"])));
            text.push_str(&format!("| 期间含押金的付款累计 | {} | 没有扣除未知退款；不是开始时一次付清 |\n",amount(&horizon["listed_cash_outflow_before_deposit_return"])));
        }
        if cost["cash_after_listed_upfront"].is_number() {
            text.push_str(&format!("| 仅扣已列前期项目后的现金 | {} | 不是完整现金余额，未计入缺项 |\n",amount(&cost["cash_after_listed_upfront"])));
        }
        if let Some(items)=cost["items"].as_array() {
            let missing: Vec<_> = items.iter().filter(|item|item["normalized_amount"].is_null()).map(|item|cell(item["name"].as_str().unwrap_or("未列费用"))).collect();
            if !missing.is_empty() {text.push_str(&format!("\n金额未取得：{}。这些项目没有按零计算。\n",missing.join("、")));}
        }
        if cost["costs_complete"] != true || cost["amounts_complete"] != true || cost["cash_classification_complete"] != true {
            text.push_str("\n费用尚未列全或仍有未知项，不能据此判断资金足够、经营可持续月数或净利润。\n");
        }
        let assessment=&cost["constraint_assessment"];
        if assessment["status"] == "ineligible" {
            text.push_str("\n此方案超过已声明底线，只作为报价对照，不能作为可接受的选择。\n");
        } else if assessment["status"] == "unknown" {
            text.push_str("\n此方案的底线条件尚未核清，不能作为已经可接受的选择。\n");
        }
    }
    text
}
