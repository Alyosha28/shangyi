//! `tools/list` 的载荷：把领域工具规格翻译成 MCP 的 `Tool` 对象。
//!
//! # 三条不许违反的规则
//!
//! 1. **工具清单只有一处真相源**：`ToolRegistry::standard()`。本模块不硬编码
//!    任何工具名（测试里另说——测试必须能独立数一遍，否则它只是在复述实现）。
//! 2. **schema 不伪造**：`shangyi_tools::schema::tool_schema(name)` 返回 `None`
//!    的工具，`inputSchema` 就是 `{"type": "object"}`，并在 description 末尾
//!    **如实标注**"参数结构未逐一约束"。绝不补一个看起来严谨的假 properties。
//! 3. **客户端不支持的 JSON Schema 关键字要显式降级并留痕**（见
//!    [`sanitize_for_client`]）。降级不等于编造：被省略的关键字逐条写在
//!    description 里，模型与审计都能看见"这里少了一条例约束"。

use std::collections::BTreeSet;

use serde_json::{Value, json};
use shangyi_tools::ToolRegistry;
use shangyi_tools::schema::tool_schema;

/// 无显式 schema 时的固定说明。
///
/// 措辞必须包含"未逐一约束"——它是对模型的**诚实预告**：参数结构靠描述与
/// 执行器的报错来收敛，别指望 schema 校验。
pub const UNCONSTRAINED_NOTE: &str = "（参数结构未逐一约束：本工具没有显式参数 schema，\
服务端不伪造字段；请按描述传参，参数错误会由执行器以具体原因拒绝。）";

/// 客户端 JSON Schema 子集之外的键被省略时的固定说明前缀。
pub const DOWNGRADED_PREFIX: &str = "（已省略本客户端不支持的 JSON Schema 关键字：";

/// DeepSeek Harness 的 JSON Schema 子集（`@deepseek-ai/dsh-tools` 的
/// `CONSTRAINT_KEYWORDS` + `ANNOTATION_KEYWORDS`，本机实测版本 0.2.1-rc.2）。
///
/// 为什么这层适配必须存在（实测依据）：dsh 用该子集把 schema 渲染成提示里的
/// TypeScript 类型；遇到子集外的关键字，`jsonSchemaToTs` **整条**降级为
/// `unknown`。也就是说 —— 只因为 `solve_equilibria` 用了 `minimum`，模型就
/// 连 `method` 的 enum 都看不到。省掉少数几个取值域约束，换来结构仍可读，
/// 是这里唯一说得通的取舍。
pub const CLIENT_SCHEMA_KEYWORDS: &[&str] = &[
    "type",
    "oneOf",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
    "description",
    "title",
    "default",
    "examples",
];

/// 一个可以上线到 MCP 的工具描述。
#[derive(Debug, Clone, PartialEq)]
pub struct WireTool {
    pub name: String,
    /// 已含降级说明的描述。
    pub description: String,
    pub input_schema: Value,
    /// 领域层是否给了显式 schema（`tool_schema` 非 None）。
    pub has_explicit_schema: bool,
    /// 被省略的客户端不支持关键字（有序、去重）。非空时 description 里有说明。
    pub downgraded: Vec<String>,
}

impl WireTool {
    /// MCP `Tool` 对象。
    pub fn to_mcp(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
        })
    }
}

/// 把一条工具规格翻译成线上形态。
pub fn wire_tool(name: &str, description: &str) -> WireTool {
    match tool_schema(name) {
        Some(mut schema) => {
            let mut downgraded = BTreeSet::new();
            sanitize_for_client(&mut schema, &mut downgraded);
            let downgraded: Vec<String> = downgraded.into_iter().collect();
            let mut description = description.to_string();
            if !downgraded.is_empty() {
                description.push(' ');
                description.push_str(DOWNGRADED_PREFIX);
                description.push_str(&downgraded.join("、"));
                description.push_str(
                    "；这些约束不在本客户端的 schema 子集内，取值合法性仍由执行器在调用时校验。）",
                );
            }
            WireTool {
                name: name.to_string(),
                description,
                input_schema: schema,
                has_explicit_schema: true,
                downgraded,
            }
        }
        // **不伪造**：没有 schema 就给空对象，并如实说明。
        None => WireTool {
            name: name.to_string(),
            description: format!("{description} {UNCONSTRAINED_NOTE}"),
            input_schema: json!({"type": "object"}),
            has_explicit_schema: false,
            downgraded: Vec::new(),
        },
    }
}

/// 全部工具（按注册表的字典序；注册表本身是有序 BTreeMap）。
pub fn wire_tools(registry: &ToolRegistry) -> Vec<WireTool> {
    tool_specs(registry)
        .into_iter()
        .map(|spec| wire_tool(&spec.name, &spec.description))
        .collect()
}

/// 按工具名字典序取全部规格。
///
/// 用 `names()` + `get()` 而不是另开一个迭代器：注册表的内部容器是私有的，
/// 加一个"按序取全部"的 getter 会扩大领域 crate 的公开面（本 crate 只读它）。
fn tool_specs(registry: &ToolRegistry) -> Vec<&shangyi_tools::ToolSpec> {
    registry
        .names()
        .into_iter()
        .filter_map(|name| registry.get(name))
        .collect()
}

/// schema 覆盖率统计（供测试与报告用；**不伪造**任何工具）。
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaCoverage {
    /// 有显式 schema 的工具名。
    pub with_schema: Vec<String>,
    /// 没有显式 schema、退回自由对象的工具名。
    pub without_schema: Vec<String>,
    /// 因客户端不支持而被省略的关键字 → 涉及的工具名（用于审计"降级了什么"）。
    pub downgraded: Vec<(String, Vec<String>)>,
}

/// 统计覆盖率。**从注册表逐个数**，不从任何硬编码清单。
pub fn schema_coverage(registry: &ToolRegistry) -> SchemaCoverage {
    let mut with = Vec::new();
    let mut without = Vec::new();
    let mut downgraded = Vec::new();
    for spec in tool_specs(registry) {
        let wire = wire_tool(&spec.name, &spec.description);
        if wire.has_explicit_schema {
            with.push(spec.name.clone());
        } else {
            without.push(spec.name.clone());
        }
        if !wire.downgraded.is_empty() {
            downgraded.push((spec.name.clone(), wire.downgraded.clone()));
        }
    }
    SchemaCoverage {
        with_schema: with,
        without_schema: without,
        downgraded,
    }
}

/// 递归删除客户端不支持的 schema 关键字。
///
/// 只删**关键字**，不删 `properties` 里的**字段名**——后者是数据，前者是约束。
/// 被删的键记进 `sink`，供上层写进 description（留痕）。
fn sanitize_for_client(node: &mut Value, sink: &mut BTreeSet<String>) {
    let Some(obj) = node.as_object_mut() else {
        return;
    };
    let keys: Vec<String> = obj.keys().cloned().collect();
    for key in keys {
        match key.as_str() {
            // 子 schema 容器：递归，键本身保留
            "properties" => {
                if let Some(props) = obj.get_mut("properties").and_then(Value::as_object_mut) {
                    for (_, sub) in props.iter_mut() {
                        sanitize_for_client(sub, sink);
                    }
                }
            }
            "items" | "additionalProperties" => {
                if let Some(sub) = obj.get_mut(&key) {
                    sanitize_for_client(sub, sink);
                }
            }
            "oneOf" => {
                if let Some(arr) = obj.get_mut("oneOf").and_then(Value::as_array_mut) {
                    for sub in arr.iter_mut() {
                        sanitize_for_client(sub, sink);
                    }
                }
            }
            known if CLIENT_SCHEMA_KEYWORDS.contains(&known) => {}
            other => {
                sink.insert(other.to_string());
                obj.remove(other);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_schema_becomes_free_object_without_fabricated_properties() {
        let w = wire_tool("sandbox_exec", "临时数值探索");
        assert!(!w.has_explicit_schema, "sandbox_exec 在 schema.rs 里没有专属 schema");
        assert_eq!(w.input_schema, json!({"type": "object"}));
        assert!(
            !serde_json::to_string(&w.input_schema).unwrap().contains("properties"),
            "**不得伪造** properties"
        );
        assert!(w.description.contains("未逐一约束"));
    }

    #[test]
    fn real_schema_is_kept_and_only_unsupported_keys_are_dropped() {
        let w = wire_tool("solve_equilibria", "均衡枚举与精炼");
        assert!(w.has_explicit_schema);
        let text = w.input_schema.to_string();
        // method 的 enum 是结构信息，必须保留
        assert!(text.contains("support_enumeration"), "enum 应保留：{text}");
        assert!(w.downgraded.contains(&"minimum".to_string()));
        assert!(w.description.contains("minimum"), "省略了什么必须写在描述里");
        assert!(!text.contains("\"minimum\""), "线上 schema 里不该再有 minimum");
    }

    #[test]
    fn sanitize_never_drops_property_names() {
        let mut v = json!({"type":"object","properties":{"minimum":{"type":"number"}}});
        let mut sink = BTreeSet::new();
        sanitize_for_client(&mut v, &mut sink);
        assert_eq!(v["properties"]["minimum"], json!({"type":"number"}));
        assert!(sink.is_empty());
    }
}
