//! 内置剧本（`shangyi/playbooks/`）的加载与校验测试。
//!
//! # 为什么这些测试必须存在
//!
//! 剧本是**数据**，而数据的错误不会被编译器发现。一个 YAML 拼错字段名的剧本
//! （例如把 `fails_when` 写成 `fail_when`）在 `serde` 下会**静默变成默认值**，
//! 于是"何时不该用本剧本"这一栏悄无声息地空了——而它恰恰是剧本里最重要的声明之一。
//!
//! 故本测试：
//! 1. 逐个加载四个内置剧本（路径拼错即失败）；
//! 2. 断言**关键字段非空**（而非只说"能解析"）；
//! 3. 断言三个模式的主工具都在白名单里——否则 CLI 的模式自检会在用户
//!    启动时才发现问题（那太晚了）。

use std::path::PathBuf;

use shangyi_playbooks::Playbook;

/// 仓库内剧本目录（相对本 crate）。
fn playbooks_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../playbooks")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../playbooks"))
}

fn load(id: &str) -> Playbook {
    let p = playbooks_root().join(id).join("playbook.yml");
    let text = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("读不到 {}：{e}", p.display()));
    let pb = Playbook::from_yaml(&text)
        .unwrap_or_else(|e| panic!("{} 解析失败：{e}", p.display()));
    pb.validate()
        .unwrap_or_else(|e| panic!("{} 未通过校验：{e}", p.display()));
    pb
}

const BUILTIN: [&str; 4] = [
    "pricing_response",
    "competitor_intel",
    "negotiation",
    "market_entry",
];

#[test]
fn all_builtin_playbooks_load_and_validate() {
    for id in BUILTIN {
        let pb = load(id);
        assert_eq!(pb.meta.id, id, "meta.id 必须与目录名一致（否则按 id 查找会失败）");
    }
}

/// 每个内置剧本都必须声明**失效条件**。
///
/// 这条不是形式要求：`fails_when` 是"何时不该用本剧本"，缺失它意味着
/// 剧本会在不适用的问题上被套用，而用户不会察觉。
#[test]
fn every_playbook_declares_when_it_must_not_be_used() {
    for id in BUILTIN {
        let pb = load(id);
        assert!(
            !pb.applicable_when.is_empty(),
            "`{id}` 未声明 applicable_when（何时该用）"
        );
        assert!(
            !pb.fails_when.is_empty(),
            "`{id}` 未声明 fails_when（何时**不该**用）——该栏与适用条件同等重要"
        );
    }
}

/// intake 问题必须完整：每个问题有 id 与文本，choice 型必须有选项。
///
/// `Playbook::validate` 已检查 choice 有空选项；此处额外检查**问题数不为零**——
/// 没有 intake 问题的剧本会让 agent 无从开始提问。
#[test]
fn every_playbook_has_usable_intake_questions() {
    for id in BUILTIN {
        let pb = load(id);
        assert!(!pb.intake.is_empty(), "`{id}` 没有 intake 问题，agent 无从提问");
        for q in &pb.intake {
            assert!(!q.id.trim().is_empty(), "`{id}` 有问题缺 id");
            assert!(!q.text.trim().is_empty(), "`{id}` 的问题 `{}` 缺文本", q.id);
            assert!(
                matches!(q.kind.as_str(), "choice" | "text" | "number"),
                "`{id}` 的问题 `{}` 的 kind=`{}` 不在 choice/text/number 内",
                q.id,
                q.kind
            );
            if q.kind == "choice" {
                assert!(
                    !q.options.is_empty(),
                    "`{id}` 的问题 `{}` 是 choice 但无选项",
                    q.id
                );
            }
        }
    }
}

/// 三个模式的主工具都必须在白名单里——否则用户在 CLI 里启动该模式会立刻被拒。
///
/// 这与 `shangyi-cli` 的 `self_check` 是同一条约束的两端：CLI 在启动时查，
/// 这里在**发布前**查。两端都查的理由是：白名单是数据，可以在任何时候被改坏。
#[test]
fn every_playbook_allows_all_three_mode_primary_tools() {
    // 与 `ConsultationMode::primary_tool` 一致（此处字面量重复是有意的：
    // 若那边改了名，本测试会失败并逼人来看这里）
    let primary = ["solve_equilibria", "consequence_predict", "discover_satisfice"];
    for id in BUILTIN {
        let pb = load(id);
        for tool in primary {
            assert!(
                pb.allows_tool(tool),
                "`{id}` 的白名单缺 `{tool}`——该模式下 CLI 自检会拒绝启动"
            );
        }
    }
}

/// 选择器权重必须合理：非负、有限、至少有一个 > 0。
///
/// **全为 0 是危险的**：`fuse` 会因无法归一化而报 `AllWeightsZero`，
/// 于是 select 阶段永远无法产出分布——一个数据错误变成流程死锁。
#[test]
fn selector_weights_are_usable() {
    for id in BUILTIN {
        let pb = load(id);
        // selectors 是 BTreeMap<String, f64>，解析期已保证是数字
        assert!(!pb.selectors.is_empty(), "`{id}` 未配置任何选择器权重");
        let positive = pb.selectors.values().filter(|w| **w > 0.0).count();
        assert!(positive > 0, "`{id}` 的选择器权重全为 0，融合会失败");
        for (k, w) in &pb.selectors {
            assert!(w.is_finite() && *w >= 0.0, "`{id}` 的选择器 `{k}` 权重非法：{w}");
        }
    }
}

/// `learning` 选择器必须在所有内置剧本里被**显式**关闭（权重 0）。
///
/// 差异登记 R17：该判据在 MVP 不实现。显式写 0 而不是省略，使
/// "有意关闭"与"忘了配"在数据上可区分。
#[test]
fn learning_selector_is_explicitly_disabled() {
    for id in BUILTIN {
        let pb = load(id);
        match pb.selectors.get("learning") {
            Some(0.0) => {}
            Some(w) => panic!("`{id}` 的 learning 权重应为 0（MVP 未实现），实得 {w}"),
            None => panic!(
                "`{id}` 未显式写出 learning 权重——省略会让「有意关闭」\
                 与「忘了配」无法区分（R17）"
            ),
        }
    }
}

/// 工具白名单不得为空——空白名单会让该剧本下**所有**工具调用被拒。
#[test]
fn tool_allowlist_is_not_empty() {
    for id in BUILTIN {
        let pb = load(id);
        assert!(!pb.tool_allowlist.is_empty(), "`{id}` 的工具白名单为空");
        assert!(pb.allows_tool("checkpoint"), "`{id}` 必须允许 checkpoint（铁律 2）");
    }
}

/// 未知字段的容忍度**必须是显式的**。
///
/// 设计文档 §13.3 的剧本示例含若干当前结构体未建模的段
/// （intel_requirements / solver / personas / …）。本测试钉住当前行为：
/// 它们被**接受但忽略**（serde 默认），因此**不得**写进内置剧本——
/// 写了却无效比没写更坏（读者会以为它生效）。
///
/// 若将来决定"未知字段即报错"，本测试会失败并提示改这里。
#[test]
fn unknown_sections_are_currently_ignored_not_errors() {
    let yaml = r#"
meta: {id: t, name: T}
applicable_when: [a]
fails_when: [b]
solver: {enumerate: [support_enumeration]}
personas: [{player_id: x}]
"#;
    let pb = Playbook::from_yaml(yaml).expect("未知段当前被忽略");
    assert_eq!(pb.meta.id, "t");
    assert!(pb.validate().is_ok());
    // 关键推论：因为这些段被忽略，内置剧本里**不应出现**它们
    for id in BUILTIN {
        let p = playbooks_root().join(id).join("playbook.yml");
        let text = std::fs::read_to_string(&p).unwrap();
        for ignored in ["solver:", "personas:", "intel_requirements:", "novel_moves:"] {
            assert!(
                !text.contains(ignored),
                "`{id}` 含未被建模的段 `{ignored}`——它不会生效，写了会误导读者"
            );
        }
    }
}

/// 剧本白名单**必须包含所有会产生产物的工具**——否则那些阶段走不通。
///
/// # 缺陷背景（实测 run18）
///
/// 工具经过**三层过滤**：
/// 1. 工具注册表（`ToolRegistry::standard`）
/// 2. **剧本白名单**（`Playbook::tool_allowlist`）← 缺口在这里
/// 3. 阶段白名单（`ToolSpec::phases`）
///
/// 我注册了 `recommend_build`、加了它的阶段白名单、写了阶段指引，
/// 唯独**忘了加进剧本白名单**。于是模型调用它时得到
/// 「不在剧本 pricing_response 的白名单内」——而模型看不到这个三层结构，
/// 它只知道"这个工具调不动"，于是卡在 recommend 直到预算耗尽。
///
/// 三层中任何一层漏加都会造成同样的静默失效，故用测试把"三层相容"钉住。
#[test]
fn playbook_allowlist_covers_every_producing_tool() {
    // 产生产物的工具——缺任何一个，某个阶段就不可达
    let producing_tools = [
        "participant_profile_update",
        "gamespec_build",
        "solve_equilibria",
        "select_fuse",
        "consequence_predict",
        "discover_satisfice",
        "recommend_build",
        "report_render",
        "case_store_write",
    ];
    for id in BUILTIN {
        let pb = load(id);
        let missing: Vec<&str> = producing_tools
            .iter()
            .filter(|t| !pb.allows_tool(t))
            .copied()
            .collect();
        assert!(
            missing.is_empty(),
            "剧本 `{id}` 的白名单缺以下**会产生产物**的工具：{missing:?}\n\
             它们在真实运行中会报「不在剧本白名单内」——而模型只看到\
             「这个工具调不动」，对应的阶段因此不可达。"
        );
    }
}

/// 回退提议工具也必须在白名单里（否则模型无法表达"我需要回退"）。
#[test]
fn playbook_allowlist_includes_rewind_request() {
    for id in BUILTIN {
        let pb = load(id);
        assert!(
            pb.allows_tool("request_rewind"),
            "剧本 `{id}` 缺 `request_rewind`——模型将无法表达「我需要回退到更早阶段」，\
             只能停在原地（实测 run14 正是如此）"
        );
    }
}

/// 四个内置剧本的白名单**基线必须一致**，而**按需的差异必须被解释**。
///
/// # 这条测试的意图（保持）
///
/// 剧本之间的差别应在阶段路径与选择器，不在"能用哪些工具"——
/// 无意的不一致会让某个剧本**静默地少几条能力**（模型调到工具时报
/// "不在剧本白名单内"，而用户无从知道那是笔误还是设计）。
///
/// # 判据的修订（本次）
///
/// 早先它要求**完全一致**。而 `sandbox_exec` 的差异是**有理由的**：
/// 只有声明了 `cournot_numeric`（连续策略）的剧本才需要它——
/// 设计文档 §5.3 规定连续策略走沙箱的临时数值求解。
///
/// 故判据改为：
/// 1. **基线工具**（所有剧本都应有的）必须四个剧本齐全
/// 2. **按需工具**列在 `EXPLAINED_DIFFS` 里，并**附理由**——
///    新增差异必须在这里登记，否则测试失败
///
/// 这样既保留了"防静默缺失"的原意，又允许有理由的差异——
/// 而"有理由"必须**写下来**，不能只存在于作者脑中。
#[test]
fn all_builtin_playbooks_share_the_same_tool_allowlist() {
    /// 允许的差异：(工具, 出现在哪些剧本, 为什么)
    ///
    /// 每一条都必须给出理由——**这就是本机制的全部意义**：
    /// 差异可以存在，但必须是被解释的，而不是偶然的。
    const EXPLAINED_DIFFS: &[(&str, &[&str], &str)] = &[(
        "sandbox_exec",
        &["pricing_response"],
        "只有该剧本声明了 `cournot_numeric`（连续策略），而设计文档 §5.3 \
         规定连续策略走 `sandbox_exec` 的临时数值求解。其余剧本的博弈都是\
         离散策略空间，用不到沙箱。",
    )];

    let mut sets: Vec<(String, Vec<String>)> = Vec::new();
    for id in BUILTIN {
        let mut list = load(id).tool_allowlist.clone();
        list.sort();
        sets.push((id.to_string(), list));
    }

    // ── ① 去掉已解释的差异后，剩下的必须完全一致 ──
    // 去掉"已登记为按需出现"的工具——那样剩下的就是应有的基线。
    //
    // ⚠️ 判据是"该工具在 EXPLAINED_DIFFS 里被登记过"（无论当前剧本是不是
    // 声明持有它的那个）。我第一版写反了（用 `!only_in.contains(&id)`），
    // 于是它只去掉了**非声明**剧本里的项——而差异恰好出在声明剧本里。
    let strip = |list: &[String]| -> Vec<String> {
        list.iter()
            .filter(|t| !EXPLAINED_DIFFS.iter().any(|(tool, _, _)| tool == t))
            .cloned()
            .collect()
    };

    let stripped: Vec<(String, Vec<String>)> = sets
        .iter()
        .map(|(id, l)| (id.clone(), strip(l)))
        .collect();
    let (first_id, first) = &stripped[0];
    for (id, list) in stripped.iter().skip(1) {
        let diff: Vec<&String> = first
            .iter()
            .filter(|t| !list.contains(t))
            .chain(list.iter().filter(|t| !first.contains(t)))
            .collect();
        assert!(
            diff.is_empty(),
            "剧本 `{first_id}` 与 `{id}` 的白名单在**已解释差异之外**不一致：{diff:?}\n\
             若这是有意的，请把它登记进本测试的 `EXPLAINED_DIFFS` 并**写明理由**——\
             差异可以存在，但必须是被解释的，而不是偶然的。"
        );
    }

    // ── ② 已解释的差异必须**真的**只出现在声明的那些剧本里 ──
    for (tool, only_in, reason) in EXPLAINED_DIFFS {
        for (id, list) in &sets {
            let has = list.iter().any(|t| t == tool);
            let declared = only_in.contains(&id.as_str());
            assert_eq!(
                has, declared,
                "`{tool}` 的登记说它只应在 {only_in:?} 出现（理由：{reason}），\
                 但剧本 `{id}` 的实际状态是 has={has}。\
                 登记与实际不符时，这条测试失去意义——请更新登记。"
            );
        }
    }
}
