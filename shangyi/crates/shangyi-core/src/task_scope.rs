//! User-request scope is separate from the game-theory consultation mode.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskScope {
    Research,
    Cost,
    ResearchAndCost,
    Consultation,
    Conditional,
}

impl TaskScope {
    pub fn is_brief(self) -> bool { matches!(self, Self::Research | Self::Cost | Self::ResearchAndCost) }
    pub fn needs_research(self) -> bool { matches!(self, Self::Research | Self::ResearchAndCost) }
    pub fn needs_cost(self) -> bool { matches!(self, Self::Cost | Self::ResearchAndCost) }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Research => "research", Self::Cost => "cost",
            Self::ResearchAndCost => "research_and_cost", Self::Consultation => "consultation",
            Self::Conditional => "conditional",
        }
    }
    pub fn allows_tool(self, tool: &str) -> bool {
        if tool == "complete_task" { return self.is_brief(); }
        if matches!(tool,"constraint_register"|"constraint_check") {return true;}
        !self.is_brief()
            || matches!(tool, "select_task_scope" | "complete_task" | "ask_user")
            // `poi_survey` 与 web_search 同类：都是**取公开事实**。站址类问题
        // （"周边有多少家同类门店"）在资料整理范围里就该能直接数出来——
        // 此前它不在这张表里，于是模型根本看不到这个工具，只能靠网页检索
        // 猜密度（实测：一次真实咨询里 poi_survey 被结构性排除）。
        || (self.needs_research() && matches!(tool, "web_search" | "web_fetch" | "poi_survey" | "research_record" | "research_task_plan" | "research_task_update" | "research_document_lookup"))
            || (self.needs_cost() && tool == "business_cost")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskResultStatus { Complete, Partial }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopedTaskResult {
    pub scope: TaskScope,
    pub status: TaskResultStatus,
    pub summary: String,
    pub missing: Vec<String>,
    pub source_urls: Vec<String>,
    pub retrieval_attempts: usize,
    pub cost_scenarios: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditional: Option<crate::conditional::ConditionalAnalysis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research: Option<serde_json::Value>,
}

impl ScopedTaskResult {
    pub fn is_complete(&self) -> bool { self.status == TaskResultStatus::Complete && self.scope!=TaskScope::Conditional }
}

/// Detect an unambiguous single-line answer whose intended Markdown layout was
/// escaped before JSON serialization. This validates only generated delivery
/// text; it never rewrites input, quoted facts, paths or code literals.
pub(crate) fn has_double_escaped_layout(summary: &str) -> bool {
    if summary.contains(['\n', '\r']) { return false; }
    let bytes=summary.as_bytes();
    fn escaped_break(bytes:&[u8], mut at:usize)->Option<usize> {
        if bytes.get(at)!=Some(&b'\\') {return None;}
        while bytes.get(at)==Some(&b'\\') {at+=1;}
        (bytes.get(at)==Some(&b'n')).then_some(at+1)
    }
    let mut at=0;let mut markers=0;
    while at<bytes.len() {
        if bytes[at]==b'`' {
            let start=at;
            while bytes.get(at)==Some(&b'`') {at+=1;}
            let ticks=at-start;
            // Backtick spans/fences can intentionally explain escaped newlines.
            while at<bytes.len() {
                if bytes[at]==b'`' {
                    let close=at;
                    while bytes.get(at)==Some(&b'`') {at+=1;}
                    if at-close==ticks {break;}
                } else {at+=1;}
            }
            continue;
        }
        if let Some(next)=escaped_break(bytes,at) {
            let mut content=next;
            while matches!(bytes.get(content),Some(b' ' | b'\t')) {content+=1;}
            let paragraph=escaped_break(bytes,next).is_some();
            let markdown=match bytes.get(content) {
                Some(b'|') => true,
                Some(b'-'|b'*'|b'+'|b'>') => bytes.get(content+1)==Some(&b' '),
                Some(b'#') => {
                    while bytes.get(content)==Some(&b'#') {content+=1;}
                    bytes.get(content)==Some(&b' ')
                }
                _ => false,
            };
            if paragraph || markdown {markers+=1;}
            if markers>=2 {return true;}
            at=next;
        } else {at+=1;}
    }
    false
}

#[cfg(test)]
mod format_tests {
    use super::has_double_escaped_layout;

    #[test]
    fn rejects_repeated_escaped_paragraph_list_and_table_layout() {
        for summary in [
            r"已核实日期。\n\n- 日期：4月30日。\n- 来源：官方页面。",
            r"已核实日期。\\n\\n- 日期：4月30日。\\n- 来源：官方页面。",
            r"|项目|结果|\n|---|---|\n|日期|4月30日|",
            r"第一段。\n\n第二段。\n\n第三段。",
        ] {assert!(has_double_escaped_layout(summary),"{summary}");}
    }

    #[test]
    fn preserves_real_markdown_and_intentional_code_or_path_literals() {
        for summary in [
            "|项目|结果|\n|---|---|\n|日期|4月30日|",
            r"用户原话中的 `\n\n`、`\n- 项目` 保持原样。",
            r#"代码示例 ```rust println!("\n\n"); // \n- literal ``` 保持原样。"#,
            r"文件位于 C:\new\notes，换行标记写作 \n。",
        ] {assert!(!has_double_escaped_layout(summary),"{summary}");}
    }
}

#[cfg(test)]
mod scope_tool_tests {
    use super::*;

    /// 站址类问题在「资料整理」范围里必须能直接数周边门店。
    ///
    /// 实测依据：一次真实咨询里用户明确要求「数清 500 米内多少家同类」，
    /// 但 `poi_survey` 不在资料范围的工具表里 ⇒ 模型**看不到**这个工具，
    /// 只能靠网页检索拼密度。这条测试把那张表钉住。
    #[test]
    fn brief_research_scope_can_count_nearby_stores() {
        let scope = TaskScope::Research;
        assert!(scope.allows_tool("poi_survey"), "资料整理应能查周边门店计数");
        assert!(scope.allows_tool("web_search"));
        assert!(scope.allows_tool("web_fetch"));
        assert!(!scope.allows_tool("gamespec_build"), "资料整理不该进入建模");
        assert!(!scope.allows_tool("solve_equilibria"), "资料整理不该求解");
    }

    /// 费用范围不需要周边计数：那是资料范围的事，别把工具表放成"都能用"。
    #[test]
    fn brief_cost_scope_does_not_gain_the_store_counter() {
        let scope = TaskScope::Cost;
        assert!(!scope.allows_tool("poi_survey"));
        assert!(scope.allows_tool("business_cost"));
    }

    /// 完整咨询不受这张表限制（它走全部阶段闸门）。
    #[test]
    fn full_consultation_is_not_restricted_by_the_brief_whitelist() {
        assert!(TaskScope::Consultation.allows_tool("poi_survey"));
        assert!(TaskScope::Consultation.allows_tool("solve_equilibria"));
    }
}
