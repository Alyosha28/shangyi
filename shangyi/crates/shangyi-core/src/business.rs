//! Deterministic business calculations and source-bound research records.
use crate::clock::{Clock, SystemClock};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn number(v: &Value, key: &str) -> Result<f64, String> {
    v.get(key)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
        .ok_or_else(|| format!("{key} 必须是非负有限数值；未知不能填零"))
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("缺少 {key}"))
}

fn nearby_quotes(body: &str, quotes: &[&str], max_span: usize) -> bool {
    let mut positions = Vec::new();
    for (field, quote) in quotes.iter().enumerate() {
        if quote.chars().count() > 500 {
            return false;
        }
        let mut count = 0;
        for (start, _) in body.match_indices(quote).take(513) {
            positions.push((start, start + quote.len(), field));
            count += 1;
        }
        if count == 0 || count > 512 {
            return false;
        }
    }
    positions.sort_unstable_by_key(|p| p.0);
    for left in 0..positions.len() {
        let mut seen = vec![false; quotes.len()];
        let mut covered = 0;
        let mut end = positions[left].1;
        for (start, finish, field) in positions.iter().skip(left).copied() {
            if start - positions[left].0 > max_span {
                break;
            }
            end = end.max(finish);
            if !seen[field] {
                seen[field] = true;
                covered += 1;
            }
            if covered == quotes.len() {
                if end - positions[left].0 <= max_span {
                    return true;
                }
                break;
            }
        }
    }
    false
}

fn date_days(raw: &str) -> Option<i64> {
    let bytes = raw.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(i, b)| i != 4 && i != 7 && !b.is_ascii_digit())
    {
        return None;
    }
    let year: i64 = raw[..4].parse().ok()?;
    let month: i64 = raw[5..7].parse().ok()?;
    let day: i64 = raw[8..].parse().ok()?;
    if !(1..=12).contains(&month) || year < 1 {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day < 1 || day > max_day {
        return None;
    }
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    Some(era * 146097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year)
}

/// Quote matching proves the quoted text was retrieved, not that an interpretation is true.
pub fn research_record(args: &Value, sources: &BTreeMap<String, String>) -> Result<Value, String> {
    let kind = text(args, "kind")?;
    if !["rental", "competitor"].contains(&kind) {
        return Err("kind 仅支持 rental/competitor".into());
    }
    let url = text(args, "url")?;
    let body = sources
        .get(url)
        .ok_or("来源尚未通过 web_fetch 取得正文；搜索摘要不能登记为原文证据")?;
    let fields = args
        .get("fields")
        .and_then(Value::as_object)
        .ok_or("fields 必须是对象")?;
    for key in if kind == "rental" {
        vec![
            "address",
            "property_type",
            "transaction_type",
            "rent",
            "rent_unit",
        ]
    } else {
        vec!["name", "address", "category"]
    } {
        if !fields.contains_key(key) {
            return Err(format!(
                "缺少必需字段 {key}；无法提取时保留为线索，不登记为完整样本"
            ));
        }
    }
    let mut quotes = Vec::new();
    for (key, item) in fields {
        let quote = text(item, "quote")?;
        let value = text(item, "value")?;
        if quote.chars().count() < 4 || !body.contains(quote) || !quote.contains(value) {
            return Err(format!(
                "字段 {key} 的原值须出现在引文中，且引文须逐字存在于已抓取正文；不得用推断替换原值"
            ));
        }
        quotes.push(quote);
    }
    if kind == "rental" {
        if let Some(Value::String(date)) = args.get("published_at") {
            if date_days(date).is_none() || !body.contains(date) {
                return Err("挂牌发布时间须为原文中的真实 YYYY-MM-DD 日期，否则保持 null".into());
            }
            // A date elsewhere on a listings page does not date this storefront.
            quotes.push(date);
        }
    }
    let max_span = if kind == "rental" { 1200 } else { 2500 };
    if !nearby_quotes(body, &quotes, max_span) {
        return Err("字段引文虽都在同一网页，但未出现在同一局部记录内；不能把不同铺位或门店的字段拼成一条事实。请换取详情页或保留为未核验线索。".into());
    }
    if kind == "rental" {
        let property = text(&fields["property_type"], "value")?;
        let transaction = text(&fields["transaction_type"], "value")?;
        if !["商铺", "门面", "底商", "店铺", "店面"]
            .iter()
            .any(|s| property.contains(s))
            || !(transaction.contains("租") || transaction.contains("转让"))
            || transaction.contains("售")
        {
            return Err("只接收商铺租赁样本；出售、住宅和办公物业不得混入".into());
        }
    }
    if kind == "rental" {
        let unit = text(&fields["rent_unit"], "value")?;
        if !["月", "年", "日", "天"].iter().any(|p| unit.contains(p)) {
            return Err("租金单位必须明确计费周期；转让费不得登记成租金".into());
        }
    }
    let today = SystemClock.now_iso8601();
    let today_days = today.get(..10).and_then(date_days);
    let freshness = match args.get("published_at") {
        None | Some(Value::Null) => "undated",
        Some(Value::String(date)) => {
            if !body.contains(date) {
                return Err("发布时间必须逐字出现在原文中，否则保持 null".into());
            }
            let published = date_days(date).ok_or("发布时间须为真实的 YYYY-MM-DD 日期")?;
            match today_days {
                Some(now) if published > now => return Err("发布时间晚于当前日期".into()),
                Some(now) if now - published > if kind == "rental" { 90 } else { 180 } => {
                    "older_than_review_window"
                }
                Some(_) => "recently_published_not_verified_current",
                None => "clock_unavailable",
            }
        }
        Some(_) => return Err("发布时间须为 YYYY-MM-DD 字符串或 null".into()),
    };
    Ok(
        json!({"kind":kind,"url":url,"fields":fields,"search_scope":text(args,"search_scope")?,
        "evidence_status":"quoted_from_fetched_page","interpretation_status":"not_independently_verified",
        "freshness_review":freshness,
        "source_urls":[url],
        "published_at":args.get("published_at").cloned().unwrap_or(Value::Null),
        "note":"引文已匹配本次抓取正文；不证明报价仍有效、门店仍营业或样本覆盖完整。"}),
    )
}

fn compact_value(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Keep separate candidates when any field or publication date differs.
pub fn same_observed_candidate(a: &Value, b: &Value) -> bool {
    if a.get("kind") != b.get("kind") || a.get("published_at") != b.get("published_at") {
        return false;
    }
    let (Some(left), Some(right)) = (
        a.get("fields").and_then(Value::as_object),
        b.get("fields").and_then(Value::as_object),
    ) else {
        return false;
    };
    left.len() == right.len()
        && left.iter().all(|(key, item)| {
            let Some(other) = right.get(key) else {
                return false;
            };
            match (
                item.get("value").and_then(Value::as_str),
                other.get("value").and_then(Value::as_str),
            ) {
                (Some(x), Some(y)) => compact_value(x) == compact_value(y),
                _ => false,
            }
        })
}

pub fn business_cost(args: &Value) -> Result<Value, String> {
    // Scenario labels are a structural boundary, not verification that the model
    // has truthfully classified source text or assigned mutually exclusive terms.
    let scenario = text(args, "scenario")?;
    let scenario_id = text(args, "scenario_id")?;
    if scenario_id.len() > 80 || !scenario_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        return Err("scenario_id 须为不超过80字符的稳定字母数字标识，可含下划线或连字符；同一方案改名、补费用或改期间时保持该标识".into());
    }
    let horizon = match args.get("horizon_months") {
        None => None,
        Some(value) => Some(value.as_f64().filter(|n| n.is_finite() && *n > 0.0 && n.fract() == 0.0 && *n <= u32::MAX as f64)
            .ok_or("horizon_months 须为大于零的整月数")? as u64),
    };
    let currency = text(args, "currency")?;
    let commitment_months = match args.get("commitment_months") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_u64().filter(|months|*months>0)
            .ok_or("commitment_months 是实际计划签约整月数，不是测算窗口；未知用 null")?),
    };
    let costs = args
        .get("costs")
        .and_then(Value::as_array)
        .filter(|v| !v.is_empty())
        .ok_or("costs 不能为空")?;
    let mut upfront_cash_expense = 0.0;
    let mut deposits = 0.0;
    let mut monthly = 0.0;
    let mut upfront_opportunity = 0.0;
    let mut monthly_opportunity = 0.0;
    let mut upfront_noncash = 0.0;
    let mut monthly_noncash = 0.0;
    let mut assumptions = vec![];
    let mut items = vec![];
    let mut unclassified = vec![];
    let mut missing_amounts = vec![];
    let mut rent = 0.0;
    let mut rent_items = 0usize;
    let mut rent_known = true;
    for cost in costs {
        let name = text(cost, "name")?;
        if text(cost, "scenario")? != scenario {
            return Err("每项费用的 scenario 必须与本次唯一方案一致；互斥租金或不同方案必须分别调用 business_cost".into());
        }
        let treatment = text(cost, "treatment")?;
        if !["cash_expense", "refundable_deposit", "opportunity_cost", "noncash_expense", "unknown"].contains(&treatment) {
            return Err("treatment 必须明确为 cash_expense/refundable_deposit/opportunity_cost/noncash_expense/unknown；不能默认现金支出".into());
        }
        let amount = match cost.get("amount") {
            Some(Value::Null) => None,
            Some(_) => Some(number(cost, "amount")?),
            None => return Err("缺少 amount；已知金额须为数值，未取得金额须显式填 null，不得用 0 或 1 占位".into()),
        };
        let basis = text(cost, "basis")?;
        if !["user_provided", "public_quote", "assumption"].contains(&basis) {
            return Err("basis 必须区分 user_provided/public_quote/assumption".into());
        }
        let source = text(cost, "source")?;
        let period = text(cost, "period")?;
        let constraint_metric = cost.get("constraint_metric").and_then(Value::as_str);
        if cost.get("constraint_metric").is_some() && !matches!(constraint_metric,Some("rent"|"other")) {
            return Err("constraint_metric 须为 rent（实际租金义务）或 other，不能用任意属性绕过硬上限".into());
        }
        if constraint_metric==Some("rent") && (period=="once" || treatment!="cash_expense") {
            return Err("用于月租上限的 rent 项必须是实际现金租金，按 month/year/day_per_sqm 给原值；一次性预付款不能冒充月租".into());
        }
        if treatment == "refundable_deposit" && period != "once" {
            return Err("可退押金必须作为 once 资金占用列示，不得摊入月固定支出；退款时间与金额未知时不得推定返还".into());
        }
        let normalized = match period {
            "once" | "month" => amount,
            "year" => amount.map(|value| value / 12.0),
            "day_per_sqm" => {
                // A missing quote is not converted to a numeric total. Validate
                // supplied dimensions, but do not demand invented dimensions.
                let area = cost.get("area_sqm").map(|_| number(cost, "area_sqm")).transpose()?;
                let days = cost.get("billing_days_per_month").map(|_| number(cost, "billing_days_per_month")).transpose()?;
                if area.is_some_and(|value| value <= 0.0)
                    || days.is_some_and(|value| value <= 0.0 || value > 31.0) {
                    return Err("面积必须大于零，计费天数须显式提供且在 0 到 31 之间".into());
                }
                match amount {
                    Some(value) => Some(value
                        * area.ok_or("已知日每平方米租金须提供 area_sqm")?
                        * days.ok_or("已知日每平方米租金须提供 billing_days_per_month")?),
                    None => None,
                }
            }
            _ => return Err("period 仅支持 once/month/year/day_per_sqm".into()),
        };
        if constraint_metric==Some("rent") {
            rent_items+=1;
            if let Some(value)=normalized {rent+=value;} else {rent_known=false;}
        }
        if let Some(normalized) = normalized {
            if !normalized.is_finite() {
                return Err("费用换算溢出".into());
            }
            match (treatment, period == "once") {
                ("cash_expense", true) => upfront_cash_expense += normalized,
                ("cash_expense", false) => monthly += normalized,
                ("refundable_deposit", _) => deposits += normalized,
                ("opportunity_cost", true) => upfront_opportunity += normalized,
                ("opportunity_cost", false) => monthly_opportunity += normalized,
                ("noncash_expense", true) => upfront_noncash += normalized,
                ("noncash_expense", false) => monthly_noncash += normalized,
                ("unknown", _) => {},
                _ => unreachable!("treatment was validated above"),
            }
        }
        if basis == "assumption" {
            assumptions.push(name.to_string());
        }
        let mut item = json!({"name":name,"scenario":scenario,"treatment":treatment,"original":cost,"normalized_amount":normalized,"normalized_period":if period=="once"{"once"}else{"month"},"source":source});
        if normalized.is_none() {
            let reason = if cost.get("missing_amount_reason").is_some() {
                text(cost, "missing_amount_reason")?
            } else {
                "金额未取得，未参与已知数值合计"
            };
            item["missing_amount_reason"] = json!(reason);
            missing_amounts.push(item.clone());
        }
        if treatment == "unknown" || normalized.is_none() {
            unclassified.push(item.clone());
        }
        items.push(item);
    }
    let complete = args
        .get("costs_complete")
        .and_then(Value::as_bool)
        .ok_or("必须显式声明 costs_complete；未列成本不等于零")?;
    let classified = unclassified.is_empty();
    if complete && !classified {
        return Err("存在金额未取得或性质未知的费用时 costs_complete 必须为 false；未知金额不得用 0 或 1 占位，未知性质不能默认现金支出".into());
    }
    let upfront = upfront_cash_expense + deposits;
    let totals = [upfront, upfront_cash_expense, deposits, monthly, upfront_opportunity,
        monthly_opportunity, upfront_noncash, monthly_noncash];
    if totals.iter().any(|n| !n.is_finite()) {
        return Err("费用合计溢出".into());
    }
    let mut out = json!({"scenario_id":scenario_id,"scenario":scenario,"currency":currency,"listed_upfront_cash":upfront,"listed_monthly_fixed_cost":monthly,
        "listed_upfront_cash_expense":upfront_cash_expense,"listed_refundable_deposits":deposits,
        "listed_upfront_opportunity_cost":upfront_opportunity,"listed_monthly_opportunity_cost":monthly_opportunity,
        "listed_upfront_noncash_expense":upfront_noncash,"listed_monthly_noncash_expense":monthly_noncash,
        "cash_classification_complete":classified,"unclassified_items":unclassified,
        "amounts_complete":missing_amounts.is_empty(),"missing_amount_items":missing_amounts,
        "costs_complete":complete,"items":items,"assumptions":assumptions,
        "scope":"conditional_calculation_not_forecast","formula":"年费/12；日每平方米租金×面积×显式月计费天数；现金仅扣 cash_expense 与可退押金，机会成本与非现金费用另列",
        "note":"单一方案条件成本测算，费用明细不接收收入。押金占用现金，不等于已消耗费用，也不推定何时退回。机会成本与非现金费用不扣现金。金额未取得或性质未知项不计入已知数值小计，现金余额仅扣已知且已分类的现金支出，不是完整余额；有任意未知项时不输出资金充足、盈亏平衡或续航判断。未列费用不视为零，未知金额不得用 0 或 1 占位；公开报价与用户假设不等于已核实事实。"});
    let mut attributes=vec![crate::constraints::ConstraintAttribute {
        subject:"user".into(),metric:crate::constraints::ConstraintMetric::UpfrontCash,
        unit:currency.into(),period:"once".into(),value:if complete&&classified {Some(upfront)}else{None},
        derived_from:"business_cost.listed_upfront_cash; incomplete cost lists do not imply zero missing payments".into(),
    },crate::constraints::ConstraintAttribute {
        subject:"user".into(),metric:crate::constraints::ConstraintMetric::LeaseDuration,
        unit:"month".into(),period:"contract".into(),value:commitment_months.map(|months|months as f64),
        derived_from:"business_cost.commitment_months; horizon_months is not a contract commitment".into(),
    }];
    if rent_items>0 {
        if !rent.is_finite(){return Err("租金属性合计溢出".into());}
        attributes.push(crate::constraints::ConstraintAttribute {
            subject:"user".into(),metric:crate::constraints::ConstraintMetric::Rent,
            unit:currency.into(),period:"month".into(),value:if rent_known {Some(rent)}else{None},
            derived_from:"sum of normalized cash_expense items explicitly tagged constraint_metric=rent".into(),
        });
    }
    out["constraint_attributes"]=json!(attributes);
    out["commitment_months"]=json!(commitment_months);
    if let Some(months) = horizon {
        let recurring_cash = monthly * months as f64;
        let cash_expense = upfront_cash_expense + recurring_cash;
        let opportunity = upfront_opportunity + monthly_opportunity * months as f64;
        let noncash = upfront_noncash + monthly_noncash * months as f64;
        let cash_outflow = cash_expense + deposits;
        if [recurring_cash, cash_expense, opportunity, noncash, cash_outflow].iter().any(|x| !x.is_finite()) {
            return Err("期间合计溢出".into());
        }
        out["horizon_totals"] = json!({"months":months,
            "listed_recurring_cash_expense":recurring_cash,
            "listed_cash_expense":cash_expense,"listed_cash_outflow_before_deposit_return":cash_outflow,
            "listed_refundable_deposits":deposits,"listed_opportunity_cost":opportunity,
            "listed_noncash_expense":noncash,"complete":complete && classified,
            "formula":"期间现金费用=一次性现金费用+月现金费用×月数；付款累计另加押金且未扣未知返还；机会成本与非现金费用分别累计",
            "note":"从方案开始累计所填整月数，月费用视为固定且贯穿整个期间；不推定押金何时退回，不将期间付款总额当作开始时一次付清，也不从当前现金一次扣除所有未来月费。未取得项不计入已知小计；本结果不是净利润或实际资金轨迹。"});
    }
    if args.get("monthly_revenue_scenario").is_some()
        && (args.get("sales").is_none() || args.get("available_cash").is_none())
    {
        return Err("收入现金情景必须同时提供 sales 和 available_cash".into());
    }
    if args.get("available_cash").is_some() {
        let cash = number(args, "available_cash")?;
        let remaining = cash - upfront;
        if !remaining.is_finite() {
            return Err("可用现金计算溢出".into());
        }
        out["cash_after_listed_upfront"] = json!(remaining);
        if classified {
            out["upfront_cash_sufficient_for_listed_items"] = json!(remaining >= 0.0);
        }
    }
    if let Some(s) = args.get("sales") {
        let margin = number(s, "contribution_margin_rate")?;
        let price = number(s, "average_ticket")?;
        let days = number(s, "open_days_per_month")?;
        if margin <= 0.0 || margin > 1.0 || price <= 0.0 || days <= 0.0 || days > 31.0 {
            return Err("贡献毛利率须在 (0,1]，客单价大于零，营业天数在 (0,31]".into());
        }
        let basis = text(s, "basis")?;
        if !["user_provided", "assumption"].contains(&basis) {
            return Err("sales.basis 须为 user_provided 或 assumption".into());
        }
        text(s, "source")?;
        out["sales_inputs"] = s.clone();
        // Keep valid inputs visible, but do not turn unclassified costs into a
        // zero-cost assumption in a coverage or cash-runway result.
        if !classified {
            if args.get("monthly_revenue_scenario").is_some() {
                number(args, "monthly_revenue_scenario")?;
            }
            return Ok(out);
        }
        let revenue = monthly / margin;
        if !revenue.is_finite() || !(revenue / price / days).is_finite() {
            return Err("盈亏平衡计算溢出".into());
        }
        out["break_even"] = json!({"monthly_revenue":revenue,"daily_orders":(revenue/price/days).ceil(),
            "metric":"listed_cash_fixed_cost_coverage",
            "basis":if complete{"declared_complete_costs"}else{"listed_costs_only_lower_bound"},
            "formula":"已列月现金固定支出/贡献毛利率；日订单数=向上取整(月收入/客单价/月营业天数)",
            "note":"此门槛只覆盖已列现金固定支出，不含机会成本、非现金费用和押金。贡献毛利率必须已扣除随销售变化的成本，不得用未扣平台费等的毛利率代替。"});
        if args.get("monthly_revenue_scenario").is_some() {
            let revenue = number(args, "monthly_revenue_scenario")?;
            let available = number(args, "available_cash")?;
            let balance = available - upfront;
            let burn = monthly - revenue * margin;
            if !balance.is_finite()
                || !burn.is_finite()
                || (burn > 0.0 && !(balance / burn).is_finite())
            {
                return Err("现金情景计算溢出".into());
            }
            out["cash_scenario"] = json!({"monthly_revenue_assumption":revenue,"cash_after_upfront":balance,
                "monthly_burn":burn,"runway_months":if balance<0.0 {Some(0.0)}else if burn>0.0{Some(balance/burn)}else{None},
                "status":if balance<0.0{"insufficient_upfront_cash"}else if burn>0.0{"cash_depletes"}else{"no_depletion_in_this_scenario"},
                "note":"简化稳态情景，不含未输入费用、收入爬坡与收付款时差；不是实际存续期预测。"});
        }
    }
    fn finite(v: &Value) -> bool {
        match v {
            Value::Number(n) => n.as_f64().is_some_and(f64::is_finite),
            Value::Array(a) => a.iter().all(finite),
            Value::Object(o) => o.values().all(finite),
            _ => true,
        }
    }
    if !upfront.is_finite() || !monthly.is_finite() || !finite(&out) {
        return Err("计算溢出".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rental_quote_requires_retrieved_body_and_matching_values() {
        let args = json!({"kind":"competitor","url":"https://example.org","search_scope":"测试街区","fields":{
   "name":{"value":"甲店","quote":"甲店位于测试街"},"address":{"value":"测试街","quote":"甲店位于测试街"},"category":{"value":"咖啡","quote":"主营咖啡饮品"}}});
        assert!(research_record(&args, &BTreeMap::new()).is_err());
        let sources = BTreeMap::from([(
            "https://example.org".into(),
            "甲店位于测试街，主营咖啡饮品".into(),
        )]);
        assert!(research_record(&args, &sources).is_ok());
        let mut bad = args.clone();
        bad["fields"]["name"]["value"] = json!("伪造店名");
        assert!(research_record(&bad, &sources).is_err());
    }
    #[test]
    fn lease_transfer_with_monthly_rent_is_not_a_property_sale() {
        let body = "示例街商铺转让，临街门面，月租10000元/月，转让费另列";
        let field = |value: &str| json!({"value":value,"quote":body});
        let mut args = json!({"kind":"rental","url":"https://example.org","search_scope":"示例街","fields":{
            "address":field("示例街"),"property_type":field("临街门面"),"transaction_type":field("转让"),"rent":field("10000"),"rent_unit":field("元/月")}});
        let sources = BTreeMap::from([("https://example.org".into(), body.into())]);
        assert!(research_record(&args, &sources).is_ok());
        args["fields"]["rent_unit"] = field("转让费");
        assert!(research_record(&args, &sources).is_err());
        let cash=business_cost(&json!({"scenario_id":"test-scenario","scenario":"购买设备","currency":"CNY","costs_complete":false,"available_cash":20000,
            "costs":[{"scenario":"购买设备","treatment":"cash_expense","name":"设备","amount":10000,"period":"once","basis":"assumption","source":"测试"}]})).unwrap();
        assert_eq!(cash["cash_after_listed_upfront"], 10000.0);
        assert!(cash.get("cash_scenario").is_none());
    }

    #[test]
    fn fields_from_distant_listings_cannot_be_combined() {
        let body = format!(
            "示例街甲商铺出租临街门面。{}另一条乙商铺月租10000元/月。",
            "无关内容".repeat(400)
        );
        let source = BTreeMap::from([("https://example.org".into(), body)]);
        let args = json!({"kind":"rental","url":"https://example.org","search_scope":"示例街","fields":{
            "address":{"value":"示例街甲","quote":"示例街甲商铺"},
            "property_type":{"value":"临街门面","quote":"出租临街门面"},
            "transaction_type":{"value":"出租","quote":"商铺出租临街"},
            "rent":{"value":"10000","quote":"月租10000元/月"},
            "rent_unit":{"value":"元/月","quote":"10000元/月"}
        }});
        let error = research_record(&args, &source).unwrap_err();
        assert!(error.contains("不同铺位"), "{error}");
    }

    #[test]
    fn date_elsewhere_on_a_rental_listing_page_does_not_date_the_storefront() {
        let listing = "示例街甲商铺出租临街门面，月租10000元/月";
        let body = format!("2026-09-20 首页公告。{}{}", "无关内容".repeat(400), listing);
        let source = BTreeMap::from([("https://example.org".into(), body)]);
        let field = |value: &str| json!({"value":value,"quote":listing});
        let args = json!({"kind":"rental","url":"https://example.org","search_scope":"示例街","published_at":"2026-09-20","fields":{
            "address":field("示例街甲"),"property_type":field("临街门面"),
            "transaction_type":field("出租"),"rent":field("10000"),"rent_unit":field("元/月")
        }});
        assert!(
            research_record(&args, &source)
                .unwrap_err()
                .contains("同一局部记录")
        );
    }

    #[test]
    fn publication_date_is_validated_and_historical_data_is_labeled() {
        let body = "2023-06-12 示例街甲店主营咖啡，地址示例街一号";
        let field = |value: &str| json!({"value":value,"quote":body});
        let sources = BTreeMap::from([("https://example.org".into(), body.into())]);
        let mut args = json!({"kind":"competitor","url":"https://example.org","search_scope":"示例街","published_at":"2023-06-12","fields":{
            "name":field("甲店"),"address":field("示例街一号"),"category":field("咖啡")
        }});
        assert_eq!(
            research_record(&args, &sources).unwrap()["freshness_review"],
            "older_than_review_window"
        );
        args["published_at"] = json!("2023-02-30");
        assert!(research_record(&args, &sources).is_err());
        args["published_at"] = Value::Null;
        assert_eq!(
            research_record(&args, &sources).unwrap()["freshness_review"],
            "undated"
        );
    }

    #[test]
    fn candidates_deduplicate_only_when_all_original_fields_and_date_match() {
        let fields = json!({"address":{"value":"示例街一号"},"rent":{"value":"1 万/月"}});
        let a = json!({"kind":"rental","published_at":"2026-09-28","fields":fields});
        let b = json!({"kind":"rental","published_at":"2026-09-28","fields":{"address":{"value":"示例街 一号"},"rent":{"value":"1万/月"}}});
        assert!(same_observed_candidate(&a, &b));
        let mut changed = b.clone();
        changed["fields"]["rent"]["value"] = json!("2万/月");
        assert!(!same_observed_candidate(&a, &changed));
        changed = b;
        changed["published_at"] = json!("2026-08-01");
        assert!(!same_observed_candidate(&a, &changed));
        let decimal = json!({"kind":"rental","published_at":"2026-09-28","fields":{
            "address":{"value":"示例街一号"},"rent":{"value":"1.5万/月"}
        }});
        let fifteen = json!({"kind":"rental","published_at":"2026-09-28","fields":{
            "address":{"value":"示例街一号"},"rent":{"value":"15万/月"}
        }});
        assert!(!same_observed_candidate(&decimal, &fifteen));
    }

    #[test]
    fn cash_costs_and_break_even_are_separate() {
        let args = json!({"scenario_id":"test-scenario","scenario":"承租报价铺位","currency":"CNY","costs_complete":false,"costs":[
   {"scenario":"承租报价铺位","treatment":"cash_expense","name":"租金","amount":5,"period":"day_per_sqm","area_sqm":100,"billing_days_per_month":30,"basis":"public_quote","source":"挂牌"},
   {"scenario":"承租报价铺位","treatment":"refundable_deposit","name":"押金","amount":30000,"period":"once","basis":"assumption","source":"押二情景"}],
   "sales":{"contribution_margin_rate":0.5,"average_ticket":50,"open_days_per_month":30,"basis":"assumption","source":"测试情景"},
   "monthly_revenue_scenario":20000,"available_cash":60000});
        let out = business_cost(&args).unwrap();
        assert_eq!(out["listed_monthly_fixed_cost"], 15000.0);
        assert_eq!(out["listed_upfront_cash"], 30000.0);
        assert_eq!(out["break_even"]["daily_orders"], 20.0);
        assert_eq!(out["cash_scenario"]["runway_months"], 6.0);
        let mut bad = args.clone();
        bad["sales"]["contribution_margin_rate"] = json!(0);
        assert!(business_cost(&bad).is_err());
        bad = args.clone();
        bad["costs"][0]
            .as_object_mut()
            .unwrap()
            .remove("billing_days_per_month");
        assert!(business_cost(&bad).is_err());
    }

    fn relocation_costs() -> Value {
        // Regression input from the live fictitious consultation on 2026-10-02:
        // the user explicitly distinguished cash payment from foregone profit.
        json!({"scenario_id":"test-scenario","scenario":"搬到备选铺位","currency":"CNY","costs_complete":false,"available_cash":150000,
            "costs":[
                {"scenario":"搬到备选铺位","treatment":"cash_expense","name":"备选铺位租金","amount":9000,"period":"month","basis":"user_provided","source":"用户虚构条件：新租金每月9000元"},
                {"scenario":"搬到备选铺位","treatment":"cash_expense","name":"搬店一次性现金支出","amount":60000,"period":"once","basis":"user_provided","source":"用户虚构条件：搬店支出6万元"},
                {"scenario":"搬到备选铺位","treatment":"refundable_deposit","name":"可退押金","amount":18000,"period":"once","basis":"user_provided","source":"用户虚构条件：押金18000元"},
                {"scenario":"搬到备选铺位","treatment":"opportunity_cost","name":"停业一个月少赚","amount":20000,"period":"once","basis":"user_provided","source":"用户虚构条件：机会损失，不是一次性现金付款"}
            ]})
    }

    #[test]
    fn relocation_cash_does_not_deduct_foregone_profit_or_treat_deposit_as_spending() {
        let result = business_cost(&relocation_costs()).unwrap();
        assert_eq!(result["scenario"], "搬到备选铺位");
        assert_eq!(result["listed_upfront_cash"], 78000.0);
        assert_eq!(result["listed_upfront_cash_expense"], 60000.0);
        assert_eq!(result["listed_refundable_deposits"], 18000.0);
        assert_eq!(result["listed_upfront_opportunity_cost"], 20000.0);
        assert_eq!(result["cash_after_listed_upfront"], 72000.0);
        assert_eq!(result["listed_monthly_fixed_cost"], 9000.0);
        assert_eq!(result["items"][3]["treatment"], "opportunity_cost");
        assert_eq!(result["cash_classification_complete"], true);
        assert!(result.get("cash_scenario").is_none());
        assert!(result.get("break_even").is_none());
    }

    #[test]
    fn duration_totals_do_not_turn_future_rent_into_upfront_cash() {
        let mut args = relocation_costs();
        args["scenario_id"] = json!("relocate");
        args["horizon_months"] = json!(24);
        let result = business_cost(&args).unwrap();
        assert_eq!(result["scenario_id"], "relocate");
        assert_eq!(result["listed_upfront_cash"], 78000.0);
        assert_eq!(result["cash_after_listed_upfront"], 72000.0);
        assert_eq!(result["horizon_totals"]["listed_cash_expense"], 276000.0);
        assert_eq!(result["horizon_totals"]["listed_cash_outflow_before_deposit_return"], 294000.0);
        assert_eq!(result["horizon_totals"]["listed_opportunity_cost"], 20000.0);
        assert_eq!(result["horizon_totals"]["complete"], false);
        args["costs"][0]["amount"] = Value::Null;
        args["sales"] = json!({"contribution_margin_rate":0.5,"average_ticket":30,"open_days_per_month":30,"basis":"assumption","source":"test"});
        let unknown = business_cost(&args).unwrap();
        assert_eq!(unknown["horizon_totals"]["listed_cash_outflow_before_deposit_return"], 78000.0);
        assert_eq!(unknown["horizon_totals"]["complete"], false);
        assert!(unknown.get("break_even").is_none());
    }

    #[test]
    fn duration_and_scenario_identity_are_explicit_and_validated() {
        for invalid in [json!(0),json!(-1),json!(1.5),Value::Null,json!("24")] {
            let mut args=relocation_costs();args["horizon_months"]=invalid;
            assert!(business_cost(&args).is_err());
        }
        let mut args=relocation_costs();args.as_object_mut().unwrap().remove("scenario_id");
        assert!(business_cost(&args).is_err());
        args["scenario_id"]=json!(" ");assert!(business_cost(&args).is_err());
        args["scenario_id"]=json!("move");args["horizon_months"]=json!(24);
        args["costs"][0]["amount"]=json!(1e308);
        assert!(business_cost(&args).unwrap_err().contains("期间合计溢出"));
    }

    #[test]
    fn classification_uses_explicit_treatment_not_item_name() {
        let mut args = relocation_costs();
        for (i, cost) in args["costs"].as_array_mut().unwrap().iter_mut().enumerate() {
            cost["name"] = json!(format!("项目 {i}"));
        }
        let result = business_cost(&args).unwrap();
        assert_eq!(result["listed_upfront_cash"], 78000.0);
        assert_eq!(result["listed_upfront_opportunity_cost"], 20000.0);
        for field in ["scenario", "treatment"] {
            let mut invalid = args.clone();
            invalid["costs"][0].as_object_mut().unwrap().remove(field);
            assert!(business_cost(&invalid).is_err(), "missing {field} must not default");
        }
        args["costs"][0]["treatment"] = json!("expense");
        assert!(business_cost(&args).is_err());
    }

    #[test]
    fn different_rent_scenarios_cannot_be_added_in_one_call() {
        let scenario = |label, rent| json!({"scenario_id":"test-scenario","scenario":label,"currency":"CNY","costs_complete":false,
            "costs":[{"scenario":label,"treatment":"cash_expense","name":"租金","amount":rent,"period":"month","basis":"assumption","source":"虚构报价"}]});
        let upper = scenario("续租每月11000元", 11000);
        let landlord = scenario("续租每月12000元", 12000);
        assert_eq!(business_cost(&upper).unwrap()["listed_monthly_fixed_cost"], 11000.0);
        assert_eq!(business_cost(&landlord).unwrap()["listed_monthly_fixed_cost"], 12000.0);
        let mut mixed = upper.clone();
        mixed["costs"].as_array_mut().unwrap().push(landlord["costs"][0].clone());
        assert!(business_cost(&mixed).unwrap_err().contains("分别调用"));
        mixed = upper.clone();
        mixed.as_object_mut().unwrap().remove("scenario");
        assert!(business_cost(&mixed).is_err());
        mixed["scenario"] = json!(" ");
        assert!(business_cost(&mixed).is_err());
    }

    #[test]
    fn recurring_non_cash_costs_stay_out_of_cash_coverage_and_runway() {
        let mut args = relocation_costs();
        args["costs"].as_array_mut().unwrap().extend([
            json!({"scenario_id":"test-scenario","scenario":"搬到备选铺位","treatment":"opportunity_cost","name":"放弃的月收益","amount":5000,"period":"month","basis":"assumption","source":"测试"}),
            json!({"scenario_id":"test-scenario","scenario":"搬到备选铺位","treatment":"noncash_expense","name":"年折旧","amount":12000,"period":"year","basis":"assumption","source":"测试"}),
        ]);
        args["sales"] = json!({"contribution_margin_rate":0.5,"average_ticket":30,"open_days_per_month":30,"basis":"assumption","source":"测试"});
        args["monthly_revenue_scenario"] = json!(10000);
        let result = business_cost(&args).unwrap();
        assert_eq!(result["listed_monthly_opportunity_cost"], 5000.0);
        assert_eq!(result["listed_monthly_noncash_expense"], 1000.0);
        assert_eq!(result["break_even"]["monthly_revenue"], 18000.0);
        assert_eq!(result["cash_scenario"]["cash_after_upfront"], 72000.0);
        assert_eq!(result["cash_scenario"]["monthly_burn"], 4000.0);
        assert_eq!(result["cash_scenario"]["runway_months"], 18.0);
    }

    #[test]
    fn unclassified_cost_is_visible_and_cannot_imply_complete_cash_results() {
        let mut args = relocation_costs();
        args["costs"][3]["treatment"] = json!("unknown");
        args["sales"] = json!({"contribution_margin_rate":0.5,"average_ticket":30,"open_days_per_month":30,"basis":"assumption","source":"测试"});
        args["monthly_revenue_scenario"] = json!(10000);
        let result = business_cost(&args).unwrap();
        assert_eq!(result["cash_classification_complete"], false);
        assert_eq!(result["unclassified_items"][0]["normalized_amount"], 20000.0);
        assert_eq!(result["cash_after_listed_upfront"], 72000.0);
        assert!(result.get("upfront_cash_sufficient_for_listed_items").is_none());
        assert!(result.get("break_even").is_none());
        assert!(result.get("cash_scenario").is_none());
        args["costs_complete"] = json!(true);
        assert!(business_cost(&args).is_err());
        args["costs_complete"] = json!(false);
        args["costs"][3]["amount"] = Value::Null;
        let result = business_cost(&args).unwrap();
        assert!(result["items"][3]["normalized_amount"].is_null());
        assert_eq!(result["unclassified_items"].as_array().unwrap().len(), 1);
        assert_eq!(result["amounts_complete"], false);
    }

    #[test]
    fn explicit_missing_amount_is_preserved_and_excluded_from_known_totals() {
        let mut args = relocation_costs();
        args["costs"].as_array_mut().unwrap().push(json!({
            "scenario":"搬到备选铺位","treatment":"cash_expense","name":"装修报价",
            "amount":null,"period":"once","basis":"user_provided","source":"用户未取得报价",
            "missing_amount_reason":"装修方尚未报价"
        }));
        args["sales"] = json!({"contribution_margin_rate":0.5,"average_ticket":30,"open_days_per_month":30,"basis":"assumption","source":"测试"});
        args["monthly_revenue_scenario"] = json!(10000);
        let result = business_cost(&args).unwrap();
        assert_eq!(result["listed_upfront_cash"], 78000.0);
        assert_eq!(result["listed_monthly_fixed_cost"], 9000.0);
        assert_eq!(result["cash_after_listed_upfront"], 72000.0);
        assert_eq!(result["amounts_complete"], false);
        assert_eq!(result["cash_classification_complete"], false);
        let missing = &result["missing_amount_items"][0];
        assert!(missing["original"]["amount"].is_null());
        assert!(missing["normalized_amount"].is_null());
        assert_eq!(missing["missing_amount_reason"], "装修方尚未报价");
        assert_eq!(result["unclassified_items"][0], *missing);
        for field in ["upfront_cash_sufficient_for_listed_items", "break_even", "cash_scenario"] {
            assert!(result.get(field).is_none(), "unknown amount must suppress {field}");
        }
        args["costs_complete"] = json!(true);
        assert!(business_cost(&args).is_err());
    }

    #[test]
    fn missing_amount_is_not_an_omitted_field_or_a_numeric_placeholder() {
        let mut args = relocation_costs();
        args["costs"] = json!([args["costs"][0].clone()]);
        args["costs"][0]["amount"] = Value::Null;
        for period in ["once", "month", "year", "day_per_sqm"] {
            args["costs"][0]["period"] = json!(period);
            let result = business_cost(&args).unwrap();
            assert!(result["items"][0]["normalized_amount"].is_null());
            assert!(!result["items"][0]["missing_amount_reason"].as_str().unwrap().is_empty());
            assert_eq!(result["amounts_complete"], false);
            assert!(result.get("upfront_cash_sufficient_for_listed_items").is_none());
        }
        args["costs"][0].as_object_mut().unwrap().remove("amount");
        assert!(business_cost(&args).unwrap_err().contains("显式填 null"));
        args["costs"][0]["period"] = json!("month");
        for invalid in [json!(-1), json!("未取得"), json!(true)] {
            args["costs"][0]["amount"] = invalid;
            assert!(business_cost(&args).is_err());
        }
        // A sourced one-yuan charge is valid; fabricating a placeholder is a
        // semantic error, not a reason to ban legitimate zero or one amounts.
        for known in [0, 1] {
            args["costs"][0]["amount"] = json!(known);
            let result = business_cost(&args).unwrap();
            assert_eq!(result["amounts_complete"], true);
            assert_eq!(result["listed_monthly_fixed_cost"], json!(known as f64));
        }
    }

    #[test]
    fn nullable_cost_amount_does_not_relax_sales_or_cash_inputs() {
        let mut args = relocation_costs();
        args["costs"][0]["amount"] = Value::Null;
        args["sales"] = json!({"contribution_margin_rate":0.5,"average_ticket":30,"open_days_per_month":30,"basis":"assumption","source":"测试"});
        args["monthly_revenue_scenario"] = json!(10000);
        for field in ["available_cash", "monthly_revenue_scenario"] {
            let mut invalid = args.clone();
            invalid[field] = Value::Null;
            assert!(business_cost(&invalid).is_err(), "{field} must still require a number");
        }
        for field in ["contribution_margin_rate", "average_ticket", "open_days_per_month"] {
            let mut invalid = args.clone();
            invalid["sales"][field] = Value::Null;
            assert!(business_cost(&invalid).is_err(), "sales.{field} must still require a number");
        }
        args["costs"][0]["treatment"] = json!("income");
        assert!(business_cost(&args).is_err(), "cost treatment does not accept income");
    }

    #[test]
    fn deposits_cannot_be_amortized_into_recurring_costs_and_sums_must_be_finite() {
        let mut args = relocation_costs();
        args["costs"][2]["period"] = json!("month");
        assert!(business_cost(&args).unwrap_err().contains("可退押金"));
        args = relocation_costs();
        args["costs"][3]["amount"] = json!(f64::MAX);
        let duplicate = args["costs"][3].clone();
        args["costs"].as_array_mut().unwrap().push(duplicate);
        assert!(business_cost(&args).unwrap_err().contains("溢出"));
    }

    #[test]
    fn constraint_attributes_use_monthly_rent_and_actual_commitment_not_the_analysis_horizon() {
        let mut args=relocation_costs();
        args["costs"][0]["constraint_metric"]=json!("rent");
        args["costs"][0]["period"]=json!("year");args["costs"][0]["amount"]=json!(144000);
        args["horizon_months"]=json!(36);args["commitment_months"]=json!(24);
        let result=business_cost(&args).unwrap();
        let attributes=result["constraint_attributes"].as_array().unwrap();
        assert_eq!(attributes.iter().find(|a|a["metric"]=="rent").unwrap()["value"],12000.0);
        assert_eq!(attributes.iter().find(|a|a["metric"]=="lease_duration").unwrap()["value"],24.0);
        args.as_object_mut().unwrap().remove("commitment_months");
        args["costs"][0]["amount"]=Value::Null;
        let missing=business_cost(&args).unwrap();
        let attributes=missing["constraint_attributes"].as_array().unwrap();
        assert!(attributes.iter().find(|a|a["metric"]=="lease_duration").unwrap()["value"].is_null());
        assert!(attributes.iter().find(|a|a["metric"]=="rent").unwrap()["value"].is_null());
        args["costs"][0]["period"]=json!("once");
        assert!(business_cost(&args).is_err(),"prepayment totals cannot masquerade as a monthly rent attribute");
    }
}
