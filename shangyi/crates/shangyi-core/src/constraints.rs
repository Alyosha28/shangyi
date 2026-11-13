//! Hard limits derived only from trusted user input, with explicit dimensions.
//! Parsing deliberately supports a small, unambiguous subset of declarations;
//! ambiguous limits block acceptance instead of becoming model-supplied numbers.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserInputFact {
    pub message_id: String,
    pub text: String,
    pub sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintMetric { Rent, UpfrontCash, LeaseDuration }

impl ConstraintMetric {
    pub fn dimensions(self) -> (&'static str, &'static str) {
        match self {
            Self::Rent => ("CNY", "month"),
            Self::UpfrontCash => ("CNY", "once"),
            Self::LeaseDuration => ("month", "contract"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConstraintEvidence {
    pub message_id: String,
    pub sequence: u64,
    pub quote: String,
    pub original_user_text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HardConstraint {
    pub id: String,
    pub subject: String,
    pub metric: ConstraintMetric,
    pub unit: String,
    pub period: String,
    pub upper_bound: f64,
    pub revision: u64,
    pub evidence: ConstraintEvidence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnresolvedUserConstraint {
    pub message_id: String,
    pub quote: String,
    pub metric: Option<ConstraintMetric>,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct ConstraintBook {
    inputs: Vec<UserInputFact>,
    active: BTreeMap<ConstraintMetric, HardConstraint>,
    history: Vec<HardConstraint>,
    unresolved: Vec<UnresolvedUserConstraint>,
    revision: u64,
    needs_review: bool,
}

impl ConstraintBook {
    pub fn user_inputs(&self) -> &[UserInputFact] { &self.inputs }
    pub fn revision(&self) -> u64 { self.revision }
    pub fn needs_review(&self) -> bool { self.needs_review }
    pub fn has_limits(&self) -> bool { !self.active.is_empty() || !self.unresolved.is_empty() }
    pub fn mark_reviewed(&mut self) { self.needs_review = false; }

    /// This API is called exclusively by real user-IO boundaries, never a tool.
    /// A model's `source=user` claim cannot add evidence to this ledger.
    pub fn record_user_input(&mut self, text: &str, actor: &str) -> bool {
        if actor != "user" || text.trim().is_empty() { return false; }
        let sequence = self.inputs.len() as u64 + 1;
        let fact = UserInputFact { message_id:format!("user-{sequence}"),text:text.to_string(),sequence };
        self.inputs.push(fact.clone());
        let mut changed = false;
        let mut message_limits=BTreeMap::new();
        let mut conflicted_metrics=std::collections::BTreeSet::new();
        for clause in clauses(text) {
            let Some(parsed) = parse_declaration(clause, text) else { continue; };
            match parsed {
                Ok((metric, upper_bound)) => {
                    if conflicted_metrics.contains(&metric) || message_limits.insert(metric,upper_bound).is_some_and(|prior|prior!=upper_bound) {
                        conflicted_metrics.insert(metric);
                        self.unresolved.push(UnresolvedUserConstraint{message_id:fact.message_id.clone(),quote:clause.to_string(),metric:Some(metric),reason:"同一条用户消息对同一指标给出不同上限，不能擅自选择较宽的一个；请用户澄清当前底线".into()});
                        self.revision+=1;changed=true;continue;
                    }
                    // A later explicit declaration can clarify an earlier limit
                    // for this metric. Other unresolved limits remain unresolved.
                    let old_unresolved = self.unresolved.len();
                    self.unresolved.retain(|item| item.metric != Some(metric));
                    let differs = self.active.get(&metric).is_none_or(|limit|limit.upper_bound != upper_bound);
                    if differs || old_unresolved != self.unresolved.len() {
                        self.revision += 1;
                        let previous = self.active.remove(&metric);
                        let id = previous.as_ref().map(|limit|limit.id.clone()).unwrap_or_else(||format!("constraint-{}", self.active.len()+1));
                        if let Some(previous) = previous { self.history.push(previous); }
                        let (unit,period)=metric.dimensions();
                        self.active.insert(metric,HardConstraint {
                            id,subject:"user".into(),metric,unit:unit.into(),period:period.into(),upper_bound,revision:self.revision,
                            evidence:ConstraintEvidence{message_id:fact.message_id.clone(),sequence,quote:clause.to_string(),original_user_text:text.to_string()},
                        });
                        changed = true;
                    }
                }
                Err((metric,reason)) => {
                    self.unresolved.push(UnresolvedUserConstraint{message_id:fact.message_id.clone(),quote:clause.to_string(),metric,reason});
                    self.revision += 1;
                    changed = true;
                }
            }
        }
        self.needs_review |= changed;
        changed
    }

    pub fn snapshot(&self) -> Value {
        json!({"revision":self.revision,"active":self.active.values().collect::<Vec<_>>(),
            "history":self.history,"unresolved_user_statements":self.unresolved,"needs_review":self.needs_review})
    }

    /// A tool may point at a complete recognized declaration, not supply a
    /// number or quote a broader passage and freely choose one of its amounts.
    pub fn confirm_quote(&self, quote: &str) -> Result<Value,String> {
        let matching:Vec<_>=self.active.values().filter(|limit|limit.evidence.quote==quote).collect();
        match matching.as_slice() {
            [limit] => Ok(json!({"constraint":limit,"revision":self.revision,"source":"trusted_user_input"})),
            _ => Err("此引文不是当前真实用户输入中可唯一识别的完整上限语句。请引用原句；不得从含报价与上限的整段中挑数，也不得用网页或模型陈述授权。语义/单位不明确时，用普通话请用户澄清。".into()),
        }
    }

    pub fn assess(&self, attributes: &[ConstraintAttribute]) -> ConstraintAssessment {
        let mut violations=Vec::new();let mut unknown=Vec::new();
        for unresolved in &self.unresolved {
            unknown.push(json!({"user_message_id":unresolved.message_id,"quote":unresolved.quote,"reason":unresolved.reason}));
        }
        for limit in self.active.values() {
            let matches:Vec<_>=attributes.iter().filter(|a|a.subject==limit.subject && a.metric==limit.metric).collect();
            match matches.as_slice() {
                [attribute] if attribute.unit==limit.unit && attribute.period==limit.period => {
                    match attribute.value.filter(|v|v.is_finite() && *v>=0.0) {
                        Some(value) if value>limit.upper_bound => violations.push(json!({"constraint_id":limit.id,"metric":limit.metric,"value":value,"upper_bound":limit.upper_bound,"unit":limit.unit,"period":limit.period})),
                        Some(_) => {},
                        None => unknown.push(json!({"constraint_id":limit.id,"reason":"方案此项金额/期间未取得，不能视为符合底线"})),
                    }
                }
                _ => unknown.push(json!({"constraint_id":limit.id,"reason":"缺少唯一、主体/单位/期间一致的方案属性；不能凭名称或文字理由判定可接受"})),
            }
        }
        ConstraintAssessment {
            revision:self.revision,
            status:if !violations.is_empty(){Acceptance::Ineligible}else if !unknown.is_empty(){Acceptance::Unknown}else{Acceptance::Eligible},
            violations,unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConstraintAttribute {
    pub subject: String,
    pub metric: ConstraintMetric,
    pub unit: String,
    pub period: String,
    pub value: Option<f64>,
    pub derived_from: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Acceptance { Eligible, Ineligible, Unknown }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConstraintAssessment {
    pub revision: u64,
    pub status: Acceptance,
    pub violations: Vec<Value>,
    pub unknown: Vec<Value>,
}

fn clauses(text:&str)->Vec<&str> {
    let mut out=Vec::new();let mut start=0;
    for (at,ch) in text.char_indices() {
        let numeric_comma=ch==',' && text.as_bytes().get(at.wrapping_sub(1)).is_some_and(u8::is_ascii_digit)
            && text.as_bytes().get(at+1).is_some_and(u8::is_ascii_digit);
        if matches!(ch,'，'|','|'。'|'；'|';'|'\n'|'！'|'？') && !numeric_comma {
            let clause=text[start..at].trim();if !clause.is_empty(){out.push(clause);}
            start=at+ch.len_utf8();
        }
    }
    let tail=text[start..].trim();if !tail.is_empty(){out.push(tail);}out
}

pub fn quote_is_complete_clause(text:&str,quote:&str)->bool {
    let clause=quote.trim().trim_end_matches(['。','；',';','，',',','！','!']).trim();
    clauses(text).contains(&clause) && text.match_indices(quote).any(|(at,_)| {
        let after=text[at+quote.len()..].trim_start();
        !after.starts_with(['?', '？'])
    })
}

type ParsedLimit=Result<(ConstraintMetric,f64),(Option<ConstraintMetric>,String)>;

/// A narrowly quoted numerical claim must contain one quantity. Broad passages
/// with dates, alternative quotes or several amounts cannot authorize picking one.
pub fn quoted_single_number(quote:&str)->Option<f64> {
    let bytes=quote.as_bytes();let mut at=0;let mut values=Vec::new();
    while at<bytes.len() {
        if !bytes[at].is_ascii_digit(){at+=1;continue;}
        let begin=at;
        while at<bytes.len() && (bytes[at].is_ascii_digit()||matches!(bytes[at],b'.'|b',')){at+=1;}
        let mut value=quote[begin..at].replace(',',"").parse::<f64>().ok()?;
        if begin>0&&bytes[begin-1]==b'-'{value=-value;}
        let suffix=&quote[at..];
        if suffix.starts_with('万'){value*=10000.0;}else if suffix.starts_with('千'){value*=1000.0;}
        if !value.is_finite(){return None;}values.push(value);
    }
    if values.len()==1{Some(values[0])}else{None}
}

fn parse_declaration(clause:&str, original:&str)->Option<ParsedLimit> {
    if ["问题","提问","字数","句话","段落"].iter().any(|word|clause.contains(word))
        && !["租金","月租","租期","现金","支出","初始投入"].iter().any(|word|clause.contains(word)) {
        return None; // A communication preference is not a monetary/lease limit.
    }
    let own=["我","我们","本人","本店","本企业","月租","租金","租期","一次性","初始投入","最长","最多","请把","请将"]
        .iter().any(|prefix|clause.starts_with(prefix));
    if !own {return None;}
    let markers=["不能超过","不超过","上限","最多","最高","最长"];
    let marker=markers.iter().filter_map(|m|clause.find(m).map(|at|(at,*m))).min_by_key(|(at,_)|*at)?;
    // A user's report of somebody else's quote is retained as user evidence,
    // but does not change or make ambiguous an already-declared personal cap.
    let before_marker=&clause[..marker.0];
    if ["是房东报价","是对方报价","是供应商报价","不是我的上限","并非我的上限","不代表我的上限"]
        .iter().any(|word|clause.contains(word))
        || (before_marker.contains("收到") && before_marker.contains('说'))
        || before_marker.contains("邮件写") || before_marker.contains("听到") || before_marker.contains("原话") {
        return None;
    }
    let metric=if clause.contains("租期") || clause.contains("签约") || clause.contains("签") && (clause.contains('年')||clause.contains("个月")) {
        Some(ConstraintMetric::LeaseDuration)
    } else if clause.contains("一次性现金") || clause.contains("一次性支出") || clause.contains("初始投入") {
        Some(ConstraintMetric::UpfrontCash)
    } else if clause.contains("租金") || clause.contains("月租")
        || (clause.starts_with('我') && original.contains("租金") && (original.contains("每月")||original.contains("元/月"))) {
        Some(ConstraintMetric::Rent)
    } else {None};
    let fail=|reason:&str|Some(Err((metric,reason.to_string())));
    let Some(metric)=metric else {return fail("这条上限的指标或期间尚不明确，请用普通话说明限制的是哪项费用或租期");};
    let mut declaration_prefix=clause[..marker.0].trim();
    for directive in ["请把","请将"] {if let Some(rest)=declaration_prefix.strip_prefix(directive){declaration_prefix=rest;break;}}
    for subject in ["我们","本人","本店","本企业","我"] {if let Some(rest)=declaration_prefix.strip_prefix(subject){declaration_prefix=rest.trim().trim_start_matches('的').trim();break;}}
    if !["","月租","月租金","每月租金","每个月租金","每年租金","租金","租期","签约期限","一次性现金支出","一次性支出","初始投入"]
        .contains(&declaration_prefix)
        || ["房东","对方","供应商","报价","不是","并非","不代表","预估","听到","收到","写着","邮件"]
            .iter().any(|word|clause.contains(word)) {
        return fail("这条语句含转述、报价、否定或非直接声明，不能当作用户本人授权的硬上限");
    }
    if ["不确定","也许","假设","如果","例如","据说","听说","不低于","至少"].iter().any(|word|clause.contains(word)) {
        return fail("这条语句是条件、举例或未确认表述，不能当作已授权硬上限");
    }
    let mut tail=&clause[marker.0+marker.1.len()..];
    for change in ["改为","改成","调整为","调整到"] {
        if let Some((_,after))=tail.rsplit_once(change) {tail=after;break;}
    }
    let Some(begin)=tail.find(|c:char|c.is_ascii_digit()) else {return fail("上限未含可直接核对的数值，请用户自然说明明确金额/时长，不代填数值");};
    if tail[..begin].contains(['-', '+', '负']) {return fail("上限不能使用带符号的相对金额，请明确非负绝对上限");}
    if !["","接受","可接受","能接受","愿意接受","为","是","控制在","设为","签","签约"].contains(&tail[..begin].trim().trim_start_matches([':', '：', '=']).trim()) {
        return fail("上限数值前存在未识别的转述或条件，不能擅自当作用户授权");
    }
    let rest=&tail[begin..];
    let end=rest.find(|c:char|!c.is_ascii_digit()&&c!='.'&&c!=',').unwrap_or(rest.len());
    let numeric=&rest[..end];let suffix=rest[end..].trim_start();
    if suffix.chars().any(|c|c.is_ascii_digit()) {return fail("同一上限语句含多个候选数值，不能自由选择其中一个");}
    if numeric.contains(',') {
        let groups:Vec<_>=numeric.split(',').collect();
        if groups.iter().skip(1).any(|part|part.len()!=3 || !part.bytes().all(|b|b.is_ascii_digit())) {return fail("金额分组不明确，不能擅自解释逗号");}
    }
    let Some(mut value)=numeric.replace(',',"").parse::<f64>().ok().filter(|v|v.is_finite()&&*v>=0.0) else {return fail("上限数值无效");};
    if metric==ConstraintMetric::LeaseDuration {
        if suffix.starts_with('年') {value*=12.0;}
        else if !(suffix.starts_with("个月")||suffix.starts_with('月')) {return fail("租期上限必须明确月或年");}
        if value.fract()!=0.0 {return fail("租期上限须可换算为整月");}
    } else {
        let foreign_units=["美元","港元","欧元","日元","英镑","澳元","加元","新加坡元","韩元","卢布","比特币","以太币","USD","HKD","EUR","JPY","GBP","AUD","CAD","SGD","KRW","RUB","BTC","ETH"];
        let explicit_cny=["人民币","CNY","RMB"].iter().any(|unit|clause.contains(unit));
        if foreign_units.iter().any(|unit|clause.contains(unit))
            || (!explicit_cny && foreign_units.iter().any(|unit|original.contains(unit))) {
            return fail("本条输入包含外币，当前上限没有可核对的人民币口径；不能默认币种或自行换汇，请用户自然说明币种");
        }
        if suffix.starts_with('万') {value*=10000.0;}
        else if suffix.starts_with('千') {value*=1000.0;}
        else if !suffix.starts_with('元') && !clause.contains("人民币") {return fail("金额须明确元、千元或万元；不能默认币种与倍数");}
        if metric==ConstraintMetric::Rent {
            if clause.contains("每年")||clause.contains("元/年") {value/=12.0;}
            else if !(clause.contains("月租")||clause.contains("每月")||clause.contains("元/月")||clause.contains("万元/月")
                ||original.contains("租金每月")||original.contains("租金每个月")) {return fail("租金上限的计费期间不明确，不能把总价当作月租");}
        }
    }
    if !value.is_finite(){return fail("上限换算溢出");}
    Some(Ok((metric,value)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rent(value:Option<f64>)->ConstraintAttribute {
        ConstraintAttribute{subject:"user".into(),metric:ConstraintMetric::Rent,unit:"CNY".into(),period:"month".into(),value,derived_from:"test receipt".into()}
    }

    #[test]
    fn only_real_user_declarations_authorize_a_bound_and_broad_quotes_do_not() {
        let mut book=ConstraintBook::default();
        let message="房东要求每月租金12000元，我的月租最多11000元";
        for actor in ["assistant","web","auto"] {assert!(!book.record_user_input(message,actor));}
        assert!(book.user_inputs().is_empty());assert!(!book.has_limits());
        assert!(book.record_user_input(message,"user"));
        assert_eq!(book.assess(&[rent(Some(12000.0))]).status,Acceptance::Ineligible);
        assert_eq!(book.assess(&[rent(Some(11000.0))]).status,Acceptance::Eligible);
        assert!(book.confirm_quote(message).is_err());
        assert!(book.confirm_quote("11000元").is_err());
        assert_eq!(book.confirm_quote("我的月租最多11000元").unwrap()["constraint"]["upper_bound"],11000.0);
    }

    #[test]
    fn upper_bound_dimensions_and_unknown_attributes_are_checked() {
        let mut book=ConstraintBook::default();
        book.record_user_input("现租金每月1万元，房东要求1.2万元。我最多接受1.1万元，租期最长3年","user");
        let mut attributes=vec![rent(Some(11000.0)),ConstraintAttribute {subject:"user".into(),metric:ConstraintMetric::LeaseDuration,unit:"month".into(),period:"contract".into(),value:Some(36.0),derived_from:"test".into()}];
        assert_eq!(book.assess(&attributes).status,Acceptance::Eligible);
        attributes[0].value=None;assert_eq!(book.assess(&attributes).status,Acceptance::Unknown);
        attributes[0]=rent(Some(11000.0));attributes[0].unit="USD".into();assert_eq!(book.assess(&attributes).status,Acceptance::Unknown);
        attributes[0]=rent(Some(11000.0));attributes[1].value=Some(37.0);assert_eq!(book.assess(&attributes).status,Acceptance::Ineligible);
        let mut independent=ConstraintBook::default();independent.record_user_input("我的月租上限9999元","user");
        assert_eq!(independent.assess(&[rent(Some(10001.0))]).status,Acceptance::Ineligible);
    }

    #[test]
    fn revisions_keep_original_evidence_and_require_new_user_authority() {
        let mut book=ConstraintBook::default();book.record_user_input("我的月租上限11000元","user");
        let revision=book.revision();book.mark_reviewed();
        book.record_user_input("我的月租上限改为13000元","assistant");assert_eq!(book.revision(),revision);
        book.record_user_input("我的月租上限改为13000元","user");
        assert!(book.revision()>revision);assert!(book.needs_review());
        assert_eq!(book.snapshot()["history"][0]["upper_bound"],11000.0);
        assert_eq!(book.snapshot()["history"][0]["evidence"]["message_id"],"user-1");
        assert_eq!(book.assess(&[rent(Some(12000.0))]).status,Acceptance::Eligible);
        assert!(book.confirm_quote("我的月租上限11000元").is_err());
    }

    #[test]
    fn ambiguous_or_conflicting_limits_are_not_relaxed_by_choosing_a_number() {
        for message in ["我的月租上限11000到12000元","我的月租上限11000元，月租上限12000元","我的月租上限11000元，月租上限12000元，月租上限12000元","我的租金上限-100元/月"] {
            let mut book=ConstraintBook::default();book.record_user_input(message,"user");
            assert_ne!(book.assess(&[rent(Some(12000.0))]).status,Acceptance::Eligible,"{message}");
            assert!(!book.snapshot()["unresolved_user_statements"].as_array().unwrap().is_empty(),"{message}");
        }
    }

    #[test]
    fn user_reporting_someone_elses_quote_does_not_authorize_a_relaxed_cap() {
        for reported in ["我收到房东邮件说月租最高12000元","月租最高12000元是房东报价，不是我的上限"] {
            let mut book=ConstraintBook::default();book.record_user_input("我的月租上限11000元","user");
            book.mark_reviewed();let revision=book.revision();
            book.record_user_input(reported,"user");
            assert_eq!(book.snapshot()["active"][0]["upper_bound"],11000.0);
            assert_ne!(book.assess(&[rent(Some(12000.0))]).status,Acceptance::Eligible);
            assert_eq!(book.assess(&[rent(Some(11000.0))]).status,Acceptance::Eligible);
            assert_eq!(book.revision(),revision);assert!(!book.needs_review());
            assert!(book.snapshot()["unresolved_user_statements"].as_array().unwrap().is_empty());
            assert!(book.confirm_quote(reported).is_err());
            assert_eq!(book.user_inputs().len(),2,"reported facts remain in the real-input ledger");
        }
    }

    #[test]
    fn question_budget_is_not_misclassified_as_a_rent_limit_from_nearby_context() {
        let mut book=ConstraintBook::default();
        book.record_user_input("现租金每月10000元，我最多回答两个问题","user");
        assert!(!book.has_limits());assert_eq!(book.user_inputs().len(),1);
    }

    #[test]
    fn ordinary_spaces_between_number_and_unit_preserve_the_exact_user_quote() {
        let mut book=ConstraintBook::default();
        let original="我 的月租上限 11000 元/月，租期最长 3 年";
        book.record_user_input(original,"user");
        assert_eq!(book.snapshot()["active"][0]["upper_bound"],11000.0);
        assert_eq!(book.snapshot()["active"][1]["upper_bound"],36.0);
        assert!(book.snapshot()["unresolved_user_statements"].as_array().unwrap().is_empty());
        assert_eq!(book.snapshot()["active"][0]["evidence"]["quote"],"我 的月租上限 11000 元/月");
        assert_eq!(book.user_inputs()[0].text,original);
        let mut ambiguous=ConstraintBook::default();ambiguous.record_user_input("我的月租上限11 000元/月","user");
        assert!(!ambiguous.snapshot()["unresolved_user_statements"].as_array().unwrap().is_empty(),"do not silently concatenate separated digits");
    }

    #[test]
    fn foreign_currency_context_cannot_silently_authorize_a_cny_limit() {
        let mut ambiguous=ConstraintBook::default();
        ambiguous.record_user_input("现租金每月1000美元，我最多接受1.1万","user");
        assert!(ambiguous.snapshot()["active"].as_array().unwrap().is_empty());
        assert!(!ambiguous.snapshot()["unresolved_user_statements"].as_array().unwrap().is_empty());
        let mut explicit=ConstraintBook::default();
        explicit.record_user_input("房东报价1000美元，我的月租上限11000元人民币","user");
        assert_eq!(explicit.snapshot()["active"][0]["unit"],"CNY");
        assert_eq!(explicit.snapshot()["active"][0]["upper_bound"],11000.0);
    }
}
