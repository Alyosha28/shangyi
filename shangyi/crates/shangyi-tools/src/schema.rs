//! 工具参数的 JSON Schema（设计文档 §7.2 工具表的落地）。
//!
//! # 为什么这个模块是承重的（实测依据）
//!
//! 早先所有工具都暴露成「自由 JSON 对象」：
//!
//! ```json
//! {"type":"object","properties":{},"additionalProperties":true}
//! ```
//!
//! 即**模型完全靠猜参数结构**。后果在一次真实咨询里非常明显：
//! `gamespec_build` 被调用 **97 次**、`gamespec_validate` 3 次，
//! 模型在试错中反复构造 GameSpec——它不是笨，是**没被告知该传什么**。
//!
//! 更糟的是它最终**放弃了建模**，并在给用户的说明里写：
//! 「`params` 参数被环境层吞掉，模型无法落盘」——一个**编造的解释**。
//! 根因只是它把参数包了一层（`{"spec": {...}}`）。
//!
//! 故本模块为每个工具给出**明确的 schema**：字段名、类型、必填项、
//! 枚举取值。它同时降低三类成本：调用次数、错误率、以及"模型编解释"的风险。
//!
//! # 与设计文档的关系
//!
//! §7.2 的表格给了「wire 名 / 阶段白名单 / 幂等」，§10.2 给了 schema 的写法示例
//! （`game_solve_spec`）。本模块把二者合到一起：**每个工具一个 schema**，
//! 字段与各执行器的实际解析一致（不一致会让 schema 变成谎言，
//! 比没有 schema 更坏——模型会照着错的写）。

use serde_json::json;

/// 取某工具的参数 schema。
///
/// **未列出的工具返回 `None`**——调用方应退回「自由对象」并**如实标注**，
/// 而不是伪造一个看起来严谨的 schema。
pub fn tool_schema(name: &str) -> Option<serde_json::Value> {
    let v = match name {
        // ── 情报与画像 ──
        "vn_map" => json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "introduce_external": {
                    "type": "boolean",
                    "description": "是否已由用户或检索引入外部力量（监管/舆论/平台）。默认为 false。"
                }
            }
        }),
        "pars_check" => json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {},
            "description": "无参数。返回 PARTS 五要素的材料完备性判定与缺口清单。"
        }),
        "ask_user" => json!({
            "type": "object",
            "description": "仅询问无法公开取得且会改变决策的内部信息、目标、偏好或约束。公开事实先用 web_search/web_fetch 核查；主体不明确时可问最小定位信息。每轮优先一个、最多两个问题，不将多个信息点塞进一道题；不重复询问已回答或表示不知道的信息。",
            "additionalProperties": false,
            "properties": {
                "questions": {
                    "type": "array",
                    "description": "筛选后确实需要用户回答的问题，最多两个。应显式提供，不要省略参数以自动发送整份剧本清单。",
                    "minItems": 1,
                    "maxItems": 2,
                    "items": {
                        "type": "object",
                        "required": ["text"],
                        "additionalProperties": false,
                        "properties": {
                            "id": {"type": "string", "description": "问题标识（用于 skip_answered）"},
                            "text": {"type": "string", "maxLength":80, "description": "问题正文：每题只问一件事，最多80字，不罗列多个信息点"},
                            "kind": {"enum": ["choice", "text", "number"], "default": "text"},
                            "options": {
                                "type": "array", "items": {"type": "string"},
                                "description": "kind=choice 时的选项"
                            }
                        }
                    }
                },
                "skip_answered": {
                    "type": "boolean",
                    "description": "只问尚未回答过的（需配合剧本清单使用）"
                }
            }
        }),
        "select_task_scope" => json!({
            "type":"object","additionalProperties":false,"required":["scope","reason"],
            "description":"intake 首先根据用户实际请求选择任务范围，不要求用户手选专业模式。仅查公开资料选research；仅算费用选cost；兼有两者选research_and_cost；明确需要策略/博弈完整分析才选consultation（此时可按问题特征同时指定 mode、playbook 与 whatif 模式的候选动作 action）。不得因为默认模式而扩大任务。",
            "properties":{
                "scope":{"enum":["research","cost","research_and_cost","consultation"]},
                "reason":{"type":"string","minLength":1,"description":"简述用户的哪些原话决定本次范围，不编造授权。"},
                "mode":{"enum":["scenario","whatif","discover"],"description":"可选：完整咨询的分析管线（scenario=情景博弈均衡推演，whatif=特定动作条件后果推演，discover=满意策略发现）。仅 intake 选 consultation 时生效。"},
                "playbook":{"enum":["pricing_response","market_entry","negotiation","competitor_intel"],"description":"可选：完整咨询匹配的行业剧本 ID（定价响应/市场进入与门店选址/商务谈判/竞对研判）。仅 intake 生效。"},
                "action":{"type":"string","minLength":1,"description":"可选：mode=whatif 时用户拟评估的具体候选动作（如「全线降价15%」）。"}
            }
        }),
        "complete_task" => json!({
            "type":"object","additionalProperties":false,"required":["summary","missing"],
            "description":"仅交付已经选择的资料/费用短任务。summary 是直接给用户的最终普通中文答复，包含实际结果、来源、时效和假设；missing 是仍未取得或未完成事项，不能为了完成而填0。引擎要求本任务已有真实检索尝试/费用结果，并将未知金额、不完整成本和抓取缺口标成partial。该工具不构建博弈、不确认假设、不宣称完整咨询已归档。",
            "properties":{"summary":{"type":"string","minLength":1,"description":"直接展示给用户的最终可读Markdown，必须遵守用户指定的格式：要求表格就提供Markdown表格。段落、列表和表格行使用真正换行字符，由JSON序列化自然转义，不要先把换行写成文字形式再重复转义。保留事实、数值、来源、未知与假设；用户原话或代码中的字面量保持原样。"},"missing":{"type":"array","items":{"type":"string","minLength":1}}}
        }),
        "begin_followup_task" => json!({
            "type": "object",
            "additionalProperties": false,
            "description": "在 `complete_task` 或 `conditional_analysis` 已完成交付后，若用户在对话中继续追问或追加新需求，调用本工具重开新一轮范围选择（保留既有证据与对话记录，清除前序任务完成锁定；之后须重新调用 `select_task_scope`）。",
            "properties": {
                "reason": {
                    "type": "string",
                    "minLength": 1,
                    "description": "可选：简述用户继续追问或追加分析的新需求。"
                }
            }
        }),
        "research_task_plan" => json!({
            "type":"object","additionalProperties":false,"required":["fields"],
            "description":"research或research_and_cost先登记本任务用户实际请求的字段，再检索。字段标签与request_quote直接摘自用户原文；对象subject也须来自用户原文。不要把用户未问的承办、指导、门牌或额外日期类型加入清单。同任务不能删字段来伪装完成。用户要求多个活动时用minimum_findings声明最少项数。",
            "properties":{"fields":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"object","additionalProperties":false,"required":["id","label","request_quote","evidence_required"],"properties":{
                "id":{"type":"string","pattern":"^[A-Za-z0-9_-]{1,64}$"},"label":{"type":"string","minLength":1,"description":"用户原文中的字段词，如日期、主办单位、发布时间；不要自行换成原文不存在的新字段"},
                "subject":{"type":"string","description":"用户原文中的对象，如整个文化节、北外滩主市集"},"request_quote":{"type":"string","minLength":1},
                "evidence_required":{"enum":["original_text","publication_time","event_date","document_issue_date","document_signature_date","source_link"]},
                "minimum_findings":{"type":"integer","minimum":1,"maximum":5}
            }}}}
        }),
        "research_task_update" => json!({
            "type":"object","additionalProperties":false,"required":["updates"],
            "description":"批量登记已登记请求field_id的结果。supported须给依据：引用已抓正文时用evidence（短引文，value逐字出现在同一引文中；搜索摘要不能当正文）；引用结构化取数（如 poi_survey 的周边计数）时用structured，只填该工具返回的 receipt 编号，数值由回执给出、不要手写——手写的数值一律拒收。发布日期、印发、落款沿用各自原文标签，不用获取时间替代。unavailable只能用实际检索/抓取凭据并选reason_code。not_applicable仅限新闻文种不适用的document_issue_date/document_signature_date，必须提供source_kind=news_article、source_kind_quote原文文种/记者标记和具体解释；主办、活动日期、发布时间等事实缺失不能借不适用绕过。不要新增用户未问的缺口。",
            "properties":{"updates":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"object","additionalProperties":false,"required":["field_id","status"],"properties":{
                "field_id":{"type":"string"},"status":{"enum":["supported","unavailable","not_applicable"]},
                "reason_code":{"enum":["source_unavailable","not_found","insufficient_evidence","provider_unavailable","budget_exhausted"]},
                "source_url":{"type":"string","description":"source_unavailable引用真实失败URL；not_applicable引用真实已读URL"},"explanation":{"type":"string","maxLength":200,"description":"仅不适用判断的具体文种理由，说明新闻为何无该项印发/落款；不能扩展请求字段"},
                "source_kind":{"enum":["news_article"]},"source_kind_quote":{"type":"string","maxLength":240,"description":"已读正文中实际新闻/记者标记的短引文；不得自称文种或将公文改标新闻"},
                "structured":{"type":"array","minItems":1,"maxItems":10,"description":"结构化查询依据：只填工具返回的回执编号（如 poi_survey 的 receipts[].receipt）。值与来源取自回执，模型不得改写或自述数值。","items":{"type":"object","additionalProperties":false,"required":["receipt"],"properties":{"receipt":{"type":"string","description":"逐字引用工具返回的回执编号"}}}},
                "evidence":{"type":"array","minItems":1,"maxItems":10,"items":{"type":"object","additionalProperties":false,"required":["value","source_url","quote"],"properties":{
                    "value":{"type":"string"},"source_url":{"type":"string"},"quote":{"type":"string","maxLength":600},"date_label":{"type":["string","null"],"description":"日期字段使用原文印发日期、发布日期、更新时间等标签，不重新命名"}
                }}}
            }}}}
        }),
        "research_document_lookup" => json!({
            "type":"object","additionalProperties":false,"required":["field_ids","document_title","site_url"],
            "description":"已经从搜索见到确切文件标题但镜像正文取不到时，先在本任务已读取的官方/原发布站点定向找同名正文。标题须逐字引用已见搜索标题，site_url须来自实际已读页面；代码合成site:域名+标题并在剩余预算内搜索、读取真实命中，不编网址、不绕robots/captcha。同一标题和站点只做一次，无结果仍保留未知。",
            "properties":{"field_ids":{"type":"array","minItems":1,"items":{"type":"string"}},"document_title":{"type":"string","minLength":1},"site_url":{"type":"string","minLength":1}}
        }),
        "research_record" => json!({
            "type":"object", "additionalProperties":false,
            "description":"仅登记已通过 web_fetch 取得正文的资料。每字段 value 必须是原文字符串，quote 必须包含 value 且逐字匹配正文；所有字段引文还须在同一局部记录，不能拼接不同挂牌或门店。rental 必须含 address/property_type/transaction_type/rent/rent_unit，competitor 必须含 name/address/category。published_at 仅可填原文中的 YYYY-MM-DD，日期不明保持 null；较早资料不能称为当前报价或营业状态。登记不证明报价有效或店铺仍在营业。",
            "required":["kind","url","fields","search_scope"],
            "properties":{
                "kind":{"enum":["rental","competitor"]}, "url":{"type":"string"},
                "search_scope":{"type":"string"}, "published_at":{"type":["string","null"]},
                "fields":{"type":"object","additionalProperties":{"type":"object","required":["value","quote"],"additionalProperties":false,"properties":{"value":{"type":"string"},"quote":{"type":"string"}}}}
            }
        }),
        "business_cost" => json!({
            "type":"object","additionalProperties":false,
            "description":"单一方案的条件成本测算，不是收益预测。每次只算一个 scenario，每项 scenario 须完全一致；续租房东报价、续租可接受上限、搬店属于不同方案，必须分别调用，绝不能把互斥租金相加。每项必须显式标注 treatment：现金费用、可退押金、机会成本、非现金费用或性质未知；period=once 只表示一次性，不能代表现金。停业少赚、放弃收益是 opportunity_cost，另列且不扣可用现金。押金占用现金但不是已消耗费用，不假设退款时点。costs 只接收费用，不接收房东未来租金等收入；未知收入保留为分析缺口，不能伪装成费用。金额未取得须显式 amount:null 并声明 costs_complete=false，可给 missing_amount_reason；绝不能用 0、1 或其他数值占位。现金输入与 sales 仍须是有来源数值。",
            "required":["operation","scenario","currency","costs","costs_complete"],
            "properties":{
                "operation":{"enum":["create","update"],"description":"首次创建方案用create，不传scenario_id，由引擎分配；用户改名、调整期间或补费用时必须update，传之前结果里的scenario_id，不能另建一份。"},
                "distinct_reason":{"type":"string","minLength":1,"description":"仅在create且已存在相同费用时，说明为什么确属另一备选对象（如另一家店）。原方案改名或重新计算不属于新对象，必须update。金额相同本身不会自动合并不同对象。"},
                "scenario_id":{"type":"string","minLength":1,"maxLength":80,"description":"仅update时必填，逐字引用当前会话此前工具返回的标识（如cost-1），不得自行发明或更改；create时不得传。"},
                "horizon_months":{"type":"integer","minimum":1,"maximum":4294967295u64,"description":"可选：从方案开始累计的整月数（两年=24）。月费仍用month/year/day_per_sqm；不得将未来所有租金伪装成period=once。返回期间合计与初始现金占用，二者分列；只支持期间内固定月费，变动租金应另行分段分析。"},
                "commitment_months":{"type":["integer","null"],"minimum":1,"description":"实际拟签合同的整月数，用于用户租期上限；不得拿horizon_months测算窗口替代。未知保持null。"},
                "scenario":{"type":"string","minLength":1,"description":"本次唯一且具体的方案名称，如续租每月11000元或搬到备选铺位；同一方案更新时保持名称一致"},
                "currency":{"type":"string"},"costs_complete":{"type":"boolean","description":"是否已列全本方案的固定费用与一次性支出，且金额与性质均明确；只列部分差额、amount:null、不确定或存在 unknown 时必须 false"},
                "costs":{"type":"array","minItems":1,"items":{"type":"object","additionalProperties":false,"required":["name","scenario","treatment","amount","period","basis","source"],"properties":{
                    "name":{"type":"string"},"amount":{"type":["number","null"],"minimum":0,"description":"已知金额填有来源的非负数值；金额未取得须显式 null，不能省略或用0/1占位。null保留在明细并排除出已知数值小计。"},
                    "missing_amount_reason":{"type":"string","minLength":1,"description":"amount=null 时可说明金额为何未取得；不要求用户为了填齐而猜数。"},
                    "scenario":{"type":"string","minLength":1,"description":"必须与顶层 scenario 完全一致；不属于本方案的费用另开调用"},
                    "treatment":{"enum":["cash_expense","refundable_deposit","opportunity_cost","noncash_expense","unknown"],"description":"cash_expense=实际现金费用；refundable_deposit=可退押金，仅允许once；opportunity_cost=未实现收益或放弃的收益，不扣现金；noncash_expense=折旧等非现金费用，不扣现金；unknown=费用性质未知，另列且不能用于资金充足/续航结论，金额也未知时使用amount:null。缺少分类会被拒绝，不从名称自动推断。"},
                    "constraint_metric":{"enum":["rent","other"],"description":"实际合同租金行标rent，按原period换算成月租用于用户硬上限；其他费用标other。存在租金底线但不提供可校验rent项会得到unknown，不能进入推荐。"},
                    "period":{"enum":["once","month","year","day_per_sqm"]},
                    "area_sqm":{"type":"number","exclusiveMinimum":0},"billing_days_per_month":{"type":"number","exclusiveMinimum":0,"maximum":31},
                    "basis":{"enum":["user_provided","public_quote","assumption"]},"source":{"type":"string"}
                }}},
                "sales":{"type":"object","additionalProperties":false,"required":["contribution_margin_rate","average_ticket","open_days_per_month","basis","source"],"properties":{
                    "contribution_margin_rate":{"type":"number","exclusiveMinimum":0,"maximum":1},"average_ticket":{"type":"number","exclusiveMinimum":0},
                    "open_days_per_month":{"type":"number","exclusiveMinimum":0,"maximum":31},"basis":{"enum":["user_provided","assumption"]},"source":{"type":"string"}
                }},
                "monthly_revenue_scenario":{"type":"number","minimum":0},"available_cash":{"type":"number","minimum":0}
            }
        }),
        "constraint_register" => json!({
            "type":"object","additionalProperties":false,"required":["quote"],
            "description":"核对真实用户已表达的硬上限。引擎在真实用户输入边界登记证据并解析明确上限；本工具只引用完整上限原句，不接收模型指定的上限数值/主体或source=user自述。不能引用整段报价和上限后挑选较宽数字。歧义、未知单位或期间必须用普通话让用户澄清；用户无需填写专业字段。修改上限只接受新的真实用户声明，旧来源与修订历史保留。",
            "properties":{"quote":{"type":"string","minLength":1,"description":"真实用户原文中的完整上限语句，逐字引用。"}}
        }),
        "conditional_analysis" => json!({
            "type":"object","additionalProperties":false,
            "description":"当完整博弈缺少真实收益、可比口径或可授权假设时，根据引擎当前事实快照交付conditional/partial分析。reason只能说明意图，不能制造缺口、替代证据或提供数值。引擎保留原参与者、原草案、已计算费用、真实用户上限及未取得项；不能删对手、填0、伪造cp2确认或冒充已求得均衡。",
            "properties":{"reason":{"type":"string","description":"说明希望按已知条件交付的意图；实际证据只从会话状态读取。"}}
        }),
        "constraint_check" => json!({
            "type":"object","additionalProperties":false,
            "description":"按最新真实用户硬上限核对已计算的方案，返回constraint_revision、eligible_scenario_ids、comparison_only_scenario_ids和unknown_scenario_ids。超限报价可保留作比较，不能进入推荐；属性未知/主体单位期间不一致不等于允许。推荐必须引用这里的当前版本和已核对scenario_id。修改底线后旧建议/报告失效，需要重新核对。",
            "properties":{"scenario_ids":{"type":"array","minItems":1,"items":{"type":"string"},"description":"已存在的方案标识；省略时核对全部。"}}
        }),
        "poi_survey" => json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["address"],
            "properties": {
                "address": {
                    "type": "string",
                    "description": "要查的地址（越具体越好，例如「上海市南京东路32号」）。先用它定位，再计数。"
                },
                "radius": {
                    "type": "integer",
                    "minimum": 100,
                    "maximum": 3000,
                    "description": "查询半径（米），默认 500。站址类问题用 300–800 就够；超范围会被钳制。"
                },
                "brand": {
                    "type": "string",
                    "description": "同品牌核查（可选）：这个品牌在该半径内已开几家。做「要不要开第 N 家」时必须查。"
                },
                "keywords": {
                    "type": "string",
                    "description": "额外关键词计数（可选）：默认类目之外还想数的业态，例如「宠物店」。"
                }
            },
            "description": "返回该半径内各类目的**门店数量**（同类密度、商场、超市、写字楼、住宅、地铁口、公交站）与同品牌存量。计数是坐标级硬事实，用来校准份额与竞品假设；它不是营业额或客流。没查到的类目会列在 unavailable —— 未取得不等于 0。"
        }),
        "web_search" => json!({
            "type": "object",
            "required": ["query"],
            "additionalProperties": false,
            "properties": {
                "query": {"type": "string", "description": "检索词。需要官方来源且域名已确认时可使用 site:域名 前缀（转为域名过滤）；排除已知无关站点可用 -site:域名。含明确时效词（本周/近期/今年…）时会收紧时间范围，历史年份请直接写出年份。结果按检索相关性排序并给出 rank_score 与 rank_signals；排序分不是事实可信度，摘要须用 web_fetch 核对原文。"}
            }
        }),
        "web_fetch" => json!({
            "type": "object",
            "description":"读取原始网页。本次咨询已经成功读取的同一URL默认复用快照，保留原抓取时间且不消耗网络预算；不会冒充刚刚重新核验。仅在确需更新时效时用refresh=true，重新联网受总预算约束。",
            "required": ["url"],
            "additionalProperties": false,
            "properties": {
                "url": {"type": "string", "description": "要抓取的 URL（http/https）"},
                "refresh":{"type":"boolean","description":"是否强制重新联网读取；默认false，可复用同会话的已读正文"}
            }
        }),
        "participant_profile_update" => json!({
            "type": "object",
            "description": "新增或更新一个参与者画像。参数**直接**是 ParticipantProfile。                            每条时间线事件都必须带来源（`DataSource`），且日期不可为空。                            \n\n可选：给 `behavior_models` 与 `type_priors` 时，引擎会做\
                            **贝叶斯类型推断**并写入 `posterior_types`（报告要用它）。\
                            不给则**不推断**，并如实标注「未推断」——因为似然模型是\
                            领域判断，引擎不能凭空造它。",
            "required": ["participant_id", "value_net_role", "relation_to_game"],
            // ⚠️ 这里曾漏掉 `behavior_models` / `type_priors`——而
            // `additionalProperties: false` 会把它们**当作非法参数拒绝**，
            // 于是"贝叶斯类型推断"即使实现了也永远无法被调用。
            //
            // 这是"能力建了但没接"在 **schema 层**的形态：模型只能从
            // schema 知道参数存在，schema 没写 = 没有这个能力。
            // 由 `tool_schemas_expose_the_parameters_the_executor_accepts` 守住。
            "additionalProperties": false,
            "properties": {
                "participant_id": {"type": "string"},
                "behavior_models": {
                    "type": "array",
                    "description": "（可选）类型推断的**似然模型**——各类型对各类行为的相对倾向。\
                        给了它就（连同 type_priors）做贝叶斯后验更新；不给则如实标注「未推断」。\
                        引擎**不替你造它**（那是领域判断，属铁律 1 禁止的编数）。",
                    "items": {
                        "type": "object",
                        "required": ["type_name"],
                        "additionalProperties": false,
                        "properties": {
                            "type_name": {"type": "string", "description": "如「清库存型」「长期战略型」"},
                            "event_weights": {
                                "type": "object",
                                "description": "事件类型 → 倾向权重（越大越可能做出该类行为）",
                                "additionalProperties": {"type": "number"}
                            },
                            "magnitude_sensitivity": {"type": "number", "default": 1.0}
                        }
                    }
                },
                "type_priors": {
                    "type": "object",
                    "description": "（可选）各类型的**先验概率，必须归一化**（和为 1）。\
                        每个有先验的类型都要有对应的 behavior_model。",
                    "additionalProperties": {"type": "number"}
                },
                "behavior_models": {
                    "type": "array",
                    "description": "（可选）各「对手类型」的行为倾向——**做贝叶斯推断必需**。                        与 `type_priors` 同时给出才会推断。每类型对各类事件的权重越大，\
                        说明该类型越可能做出那类行为（那是**领域判断**，须有依据）。",
                    "items": {
                        "type": "object",
                        "required": ["type_name", "event_weights"],
                        "additionalProperties": false,
                        "properties": {
                            "type_name": {"type": "string", "description": "如「清库存型」"},
                            "event_weights": {
                                "type": "object",
                                "description": "事件类型 → 倾向权重（键用 event_type 的取值）",
                                "additionalProperties": {"type": "number", "minimum": 0}
                            },
                            "magnitude_sensitivity": {"type": "number", "default": 1.0}
                        }
                    }
                },
                "type_priors": {
                    "type": "object",
                    "description": "（可选）各类型的先验概率，**必须归一化**（和为 1）。                        与 `behavior_models` 同时给出才会推断。",
                    "additionalProperties": {"type": "number", "minimum": 0, "maximum": 1}
                },
                "value_net_role": {
                    "enum": ["focal", "customer", "supplier", "substitutor", "complementor"]
                },
                "relation_to_game": {
                    "enum": ["player", "external_actor", "context_only"]
                },
                "common": {
                    "type": "object",
                    "description": "通用画像字段",
                    "properties": {
                        "interests": {"type": "array", "items": {"type": "string"}},
                        "resources": {"type": "array", "items": {"type": "string"}},
                        "decision_style": {"type": "string"},
                        "power_asymmetry": {"type": "string"}
                    }
                },
                "role_specific": {
                    "type": "object",
                    "description": "按 value_net_role 选对应变体。**形状是邻接标记**：\
                                    {\"role\": \"<变体名>\", \"details\": {…}}。\
                                    两者缺一即解析失败。",
                    "required": ["role", "details"],
                    "properties": {
                        "role": {
                            "enum": ["supplier", "customer", "substitutor", "complementor"],
                            "description": "变体名（小写英文）"
                        },
                        "details": {
                            "type": "object",
                            "description": "该变体的字段。例如 supplier 需 supply_concentration(number)、\
                                            switching_cost(low/medium/high)、our_batna(string)、\
                                            holdup_risk(low/medium/high)。"
                        }
                    }
                },
                "timeline": {
                    "type": "array",
                    "description": "带来源的行为事件",
                    "items": {
                        "type": "object",
                        "required": ["date", "event_type", "magnitude", "source"],
                        "properties": {
                            "date": {"type": "string", "description": "YYYY-MM-DD"},
                            "event_type": {
                                "enum": ["price_cut", "price_raise", "promotion", "new_store",
                                         "hiring", "supply_change", "statement", "other"]
                            },
                            "magnitude": {"type": "number", "minimum": 0},
                            "confidence": {"type": "number", "minimum": 0, "maximum": 1},
                            "source": {
                                "type": "object",
                                "description": "来源。kind=business 才允许（personal/unverified 构造即失败）",
                                "required": ["kind", "locator"],
                                "properties": {
                                    "kind": {"enum": ["business"]},
                                    "locator": {"type": "string", "description": "URL 或可追溯的位置描述"},
                                    "fetched_at_ms": {"type": "integer"}
                                }
                            }
                        }
                    }
                },
                "posterior_types": {"type": "object"},
                "evidence_quality": {"type": "string"}
            }
        }),
        "data_ingest" => json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "path": {"type": "string", "description": "本地 CSV/Excel 路径"},
                "hint": {"type": "string", "description": "列映射提示（哪列是什么）"}
            }
        }),

        // ── 建模 ──
        "gamespec_build" => json!({
            "type": "object",
            "description": "构建 GameSpec。参数**直接**是 GameSpec 对象（也接受包在 `spec` 键里）。\
                            校验不通过会返回逐条原因与字段清单。",
            "required": ["game_id", "players", "strategies", "payoffs", "params"],
            "additionalProperties": false,
            "properties": {
                "merge": {
                    "type": "boolean",
                    "description": "增量构建：把本次输入**深合并**进会话内已有的 GameSpec\
                                    （对象逐层合并、数组整体替换）。适合「先搭骨架、\
                                    再补参数」的两步写法——长嵌套 JSON 一次写对的\
                                    概率随长度下降，分两次写更稳。默认 false（整体替换）。"
                },
                "game_id": {"type": "string"},
                "constraint_revision":{"type":"integer","minimum":0,"description":"有真实用户硬上限时，使用constraint_check返回的当前版本；旧版本不能进入求解。"},
                "strategy_bindings":{"type":"object","description":"有硬上限时，我方每个可执行策略必须绑定已计算且eligible的方案；按player_id→策略名→scenario_id填写。超限报价仅留费用对照，不放进我方可行策略。","additionalProperties":{"type":"object","additionalProperties":{"type":"string"}}},
                "players": {
                    "type": "array",
                    "minItems": 2,
                    "description": "参与者。**每项都必须有 name**（即使与 id 相同）。",
                    "items": {
                        "type": "object",
                        "required": ["id", "name", "vn_role", "relation_to_game"],
                        "additionalProperties": false,
                        "properties": {
                            "id": {"type": "string", "description": "玩家标识，strategies 的键要用它"},
                            "name": {"type": "string", "description": "显示名（必填）"},
                            "vn_role": {
                                "enum": ["focal", "customer", "supplier", "substitutor", "complementor"],
                                "description": "价值网角色（小写英文，不要中文）"
                            },
                            "relation_to_game": {
                                "enum": ["player", "external_actor", "context_only"],
                                "description": "player=进支付矩阵；external_actor=可被引入；context_only=仅入画像"
                            },
                            "profile_ref": {"type": "string"}
                        }
                    }
                },
                "strategies": {
                    "type": "object",
                    "description": "键必须是**玩家 id**（不是 name），值为策略名数组。",
                    "additionalProperties": {"type": "array", "items": {"type": "string"}}
                },
                "payoffs": {
                    "type": "object",
                    "description": "标准式收益矩阵。形状是**邻接标记**：\
                                    {\"form\":\"normal\", \"values\":{\"cells\":{…}}}。\
                                    `cells` 必须在 `values` 里——直接挂在 `form` 下会报 \
                                    missing field values。",
                    "required": ["form", "values"],
                    "properties": {
                        "form": {"enum": ["normal", "extensive"]},
                        "values": {
                            "type": "object",
                            "description": "form=normal 时的内容",
                            "required": ["cells"],
                            "properties": {
                                "cells": {
                                    "type": "object",
                                    "description": "外层键=策略名按玩家顺序以逗号连接（如 \"降价,跟随\"）；\
                                                    内层键=玩家 id，值必须是**参数表达式**：\
                                                    {\"kind\":\"literal\",\"value\":N} 或 {\"kind\":\"ref\",\"value\":\"参数名\"}。\
                                                    裸数字会被拒绝。",
                                    "additionalProperties": {"type": "object"}
                                }
                            }
                        }
                    }
                },
                "params": {
                    "type": "object",
                    "description": "参数表。没有参数时给空对象 {}——**不能省略**。",
                    "additionalProperties": {
                        "type": "object",
                        "required": ["name", "value", "source"],
                        "properties": {
                            "name": {"type": "string"},
                            "value": {"description": "数值，或 {\"kind\":\"literal\",\"value\":N}"},
                            "source": {
                                "description": "来源。**必须写成 {\"kind\": ...} 形态**，\
                                                且各变体还有各自必填的字段（见 oneOf）。\
                                                `assumed` 不得进入求解（铁律 2）。",
                                "oneOf": [
                                    {
                                        "type": "object",
                                        "required": ["kind", "checkpoint", "user_quote"],
                                        "properties": {
                                            "kind": {"enum": ["user_confirmed"]},
                                            "user_quote":{"type":"string","description":"逐字引用真实用户对该主体、该收益数字的完整说明；不能只凭cp2标签、引用报价或把未知收益填0。执行器与实际用户输入账本核对。"},
                                            "checkpoint": {
                                                "type": "string",
                                                "description": "留档用的 checkpoint 标识，如 \"cp2\""
                                            }
                                        }
                                    },
                                    {
                                        "type":"object", "required":["kind","checkpoint","basis","user_quote"],
                                        "properties":{"kind":{"enum":["confirmed_assumption"]},"checkpoint":{"enum":["cp2"]},"basis":{"type":"string","description":"原始假设依据；接受用于计算不代表事实已证实"},"user_quote":{"type":"string","description":"真实用户明确接受此收益数值用于情景假设的原句；模型自拟假设或网页陈述不能授权。"}}
                                    },
                                    {
                                        "type": "object",
                                        "required": ["kind", "provenance"],
                                        "properties": {
                                            "kind": {"enum": ["public_data"]},
                                            "quote":{"type":"string","description":"已通过web_fetch读取且包含该主体/收益数值的原文引句；不能用搜索摘要、未读取链接或租金报价代替净收益。"},
                                            "provenance": {"type": "string", "description": "来源 URL 或工具调用 ID"}
                                        }
                                    },
                                    {
                                        "type": "object",
                                        "required": ["kind", "prior_ref"],
                                        "properties": {
                                            "kind": {"enum": ["industry_prior"]},
                                            "prior_ref": {"type": "string", "description": "先验出处；报告中会标注 borrowed"}
                                        }
                                    },
                                    {
                                        "type": "object",
                                        "required": ["kind", "note"],
                                        "properties": {
                                            "kind": {"enum": ["assumed"]},
                                            "note": {"type": "string"}
                                        }
                                    }
                                ]
                            },
                            "unit": {"type": "string"},
                            "dist": {"type": "object"}
                        }
                    }
                },
                "information": {"type": "object"},
                "dynamics": {"type": "object"},
                "external_actors": {"type": "array"},
                "preference": {"type": "object"},
                "meta": {"type": "object"}
            }
        }),
        "gamespec_validate" => json!({
            "type": "object",
            "description": "优先用 {} 校验 gamespec_build 已成功保存的当前模型，不要为了校验而重造或重发矩阵。只有要单独检查另一份完整 GameSpec 时才传 spec；这只校验该输入，不保存、不覆盖当前模型，失败也不会改为校验当前模型。成功返回 validated_target=current/provided 和实际 game_id。要修改当前模型请先 gamespec_build，再传 {} 校验。结构校验通过不代表收益口径、假设或事实已经证实。",
            "additionalProperties": false,
            "properties": {
                "spec": {
                    "type": "object",
                    "description": "仅用于单独检查完整的另一份 GameSpec；已有模型通常省略此字段，传 {} 即可。payoffs 不是玩家名到二维数组的映射，须用 form/values/cells 结构。",
                    "required": ["game_id", "players", "strategies", "payoffs", "params"],
                    "properties": {
                        "game_id": {"type": "string"},
                        "players": {"type": "array", "items": {
                            "type": "object", "required": ["id", "name", "vn_role", "relation_to_game"]
                        }},
                        "strategies": {"type": "object"},
                        "params": {"type": "object"},
                        "payoffs": {"type": "object", "required": ["form"], "description":"normal 必须包含 values.cells；extensive 仅为已有类型，不表示求解器支持。", "properties": {
                            "form": {"enum": ["normal", "extensive"]},
                            "values": {"type": "object", "required": ["cells"], "properties": {
                                "cells": {"type": "object", "description": "键为按玩家顺序排列的策略组合；每格按玩家id给参数表达式。"}
                            }}
                        }}
                    }
                }
            }
        }),

        // ── 求解与选择 ──
        "solve_equilibria" => json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "method": {
                    "enum": ["support_enumeration", "lemke_howson", "analytic"],
                    "default": "support_enumeration",
                    "description": "求解方法。默认支持枚举（唯一完备路径）。"
                },
                "refine": {
                    "enum": ["trembling_hand_perfect", "ess_symmetric", "ess_two_population"],
                    "description": "（可选）精炼过滤——用更强的解概念**筛掉**不稳健的均衡。\
                        不传则返回**全部**均衡（让 select 阶段的判据决定）。\
                        trembling_hand_perfect 对非对称博弈也适用；\
                        ess_symmetric 只对**对称**博弈有定义（用在非对称博弈上会被类型化拒绝，\
                        那时改用 ess_two_population）。"
                },
                "refine_denominator": {
                    "type": "integer",
                    "minimum": 2,
                    "maximum": 64,
                    "default": 6,
                    "description": "ESS 判据的扰动格点分辨率（仅 refine 为 ess_* 时有意义）。"
                }
            }
        }),
        "select_equilibria" => json!({
            "type": "object",
            "description": "登记某判据对每个均衡的评分。**必须给 scores**——执行器不替模型编造分数（铁律 1）。",
            "required": ["selector", "scores"],
            "additionalProperties": false,
            "properties": {
                "selector": {
                    "enum": ["risk_dom", "stochastic", "learning", "behavioral", "focal_llm", "persona_sim"]
                },
                "scores": {
                    "type": "array",
                    "description": "每个候选均衡一个分数（与 solve_equilibria 输出的顺序一致）。\
                        **推荐用对象形态**（带 rationale 与 evidence）——它们是报告的「为什么选它」，\
                        裸数字形态会在报告里退化成「未给理由」。\
                        分数不必归一化：引擎会按 `normalize_selector` 统一量纲\
                        （故计数 [90,10] 与比例 [0.9,0.1] 等价）。",
                    "items": {
                        "oneOf": [
                            {"type": "number", "minimum": 0},
                            {
                                "type": "object",
                                "required": ["score"],
                                "additionalProperties": false,
                                "properties": {
                                    "score": {"type": "number", "minimum": 0},
                                    "rationale": {
                                        "type": "string",
                                        "description": "人类可读理由——**进报告**"
                                    },
                                    "evidence": {
                                        "type": "object",
                                        "description": "机器可审计证据（收敛轨迹摘要、逐对比较数值）",
                                        "additionalProperties": {"type": "string"}
                                    }
                                }
                            }
                        ]
                    }
                }
            }
        }),
        "select_fuse" => json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {},
            "description": "无参数。融合已登记的各判据分数为均衡概率分布。"
        }),

        // ── 后果与发现 ──
        "consequence_predict" => json!({
            "type": "object",
            "required": ["decision"],
            "additionalProperties": false,
            "properties": {
                "decision": {"type": "string", "description": "我方要推演的具体动作 d"},
                "conventional": {"type": "boolean", "default": true, "description": "是否包含常规最佳反应"},
                "unconventional": {"type": "boolean", "default": true, "description": "是否包含非常规反击"}
            }
        }),
        "novel_move_filter" => json!({
            "type": "object",
            "required": ["actor_id", "description", "payoff_channel", "expected_benefit", "expected_cost"],
            "additionalProperties": false,
            "properties": {
                "actor_id": {"type": "string"},
                "description": {"type": "string"},
                "payoff_channel": {"type": "string", "description": "该行动如何改变支付结构"},
                "expected_benefit": {
                    "type": "object", "required": ["lo", "hi"],
                    "properties": {"lo": {"type": "number"}, "hi": {"type": "number"}}
                },
                "expected_cost": {
                    "type": "object", "required": ["lo", "hi"],
                    "properties": {"lo": {"type": "number"}, "hi": {"type": "number"}}
                },
                "legality_basis": {
                    "type": "array", "items": {"type": "string"},
                    "description": "合法性依据（法条/平台规则/先例）。**空数组会被三重过滤拒绝。**"
                },
                "feasibility_basis": {
                    "type": "array", "items": {"type": "string"},
                    "description": "资源可得性依据。**空数组会被拒绝。**"
                },
                "introduces_actors": {"type": "array"},
                "targets": {"type": "string"}
            }
        }),
        "monte_carlo" => json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "param": {
                    "type": "string",
                    "description": "要传播不确定性的**参数名**（推荐用法）。\
                        引擎会沿该参数在 GameSpec 里**已声明的分布**抽样、逐样本重解、\
                        汇总焦点玩家支付的 P10/P50/P90。\
                        ⚠️ 没有声明 `dist` 的参数无法传播——那是点估计，不是不确定量；\
                        引擎**不替你假设一个分布**（那会造出无来源的不确定性模型）。"
                },
                "draws": {
                    "type": "integer", "default": 200, "minimum": 1, "maximum": 2000,
                    "description": "抽样次数（种子固定 ⇒ 同输入同结果）。"
                },
                "seed": {
                    "type": "integer",
                    "description": "随机种子。固定它使结果**逐位可复现**。"
                },
                "samples": {
                    "type": "array", "items": {"type": "number"},
                    "description": "（备选）调用方自带的样本——仅在外部仿真已产出样本时使用。\
                        一般情况下应给 `param`，由引擎自己抽样。"
                }
            }
        }),
        "sensitivity_scan" => json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "param": {
                    "type": "string",
                    "description": "要扫描的**参数名**（推荐用法）。引擎会沿它取值、逐点重解、\
                        判定「均衡集合的结构」是否变化（那才是翻转判据——\
                        选哪个均衡是 select 阶段的职责，不在这里选）。"
                },
                "from": {"type": "number", "description": "扫描下界（不给则按基准值 ±50% 的默认启发式）"},
                "to": {"type": "number", "description": "扫描上界（不给则按基准值 ±50% 的默认启发式）"},
                "steps": {
                    "type": "integer", "default": 21, "minimum": 2, "maximum": 200,
                    "description": "扫描点数"
                },
                "points": {
                    "type": "array",
                    "description": "（备选）调用方自带的扫描点。一般应给 `param`，由引擎自己扫描。",
                    "items": {
                        "type": "object", "required": ["param", "flipped"],
                        "properties": {
                            "param": {"type": "number", "description": "该点的参数值"},
                            "flipped": {"type": "boolean", "description": "该参数下推荐是否翻转"}
                        }
                    }
                }
            }
        }),
        "discover_satisfice" => json!({
            "type": "object",
            "description": "满意化策略发现。输入为 DiscoveryReport 结构（候选、期望水平、各候选分位数）。",
            "additionalProperties": true,
            "properties": {
                "candidates": {"type": "array"},
                "aspiration": {"type": "object"}
            }
        }),

        // ── 知识库 ──
        "kb_retrieve" => json!({
            "type": "object",
            "description": "知识库导航检索。不给参数=取索引（推荐起点）；给 page=取该页正文与链接；\
                            给 query=关键词兜底。",
            "additionalProperties": false,
            "properties": {
                "page": {"type": "string", "description": "页 id（从索引里取）"},
                "query": {"type": "string", "description": "关键词（兜底用法）"}
            }
        }),

        // ── 案例库 ──
        "case_store_read" => json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "case_id": {"type": "string", "description": "留空则返回库的概况与校准状态"}
            }
        }),
        "case_store_write" => json!({
            "type": "object",
            "description": "归档本次咨询（冻结**此刻的预测**），或回填已归档案例的实际结果。\
                ⚠️ 预测形态**按模式而异**，三者都接受：scenario 的均衡概率分布 / \
                whatif 的条件后果报告 / discover 的满意策略集。不带预测的归档对校准无用。\
                **回填结果**（校准闭环的第五步）：给 `outcome` 键。只有「预测 + 结果」\
                成对的样本才能用于校准判据权重——没有回填的案例永远停留在待回填状态。",
            "required": ["case_id"],
            "additionalProperties": false,
            "properties": {
                "case_id": {"type": "string"},
                "date": {"type": "string", "description": "YYYY-MM-DD"},
                "question": {"type": "string", "description": "本次的决策问题"},
                "client": {"type": "string", "description": "客户匿名标识"},
                "provenance": {
                    "type": "string",
                    "enum": ["real_engagement", "exercise"],
                    "description": "**必填**：案例来源，决定它日后能否参与判据权重校准。\
                                    `real_engagement` = 真实客户委托（事后有真实结果可回填）；\
                                    `exercise` = 演练/样例/批量跑批（不会有真实结果——\
                                    其「结果」若被回填就是编的，会污染校准）。\
                                    只有 real_engagement 会被校准采纳。"
                },
                "outcome": {
                    "type": "object",
                    "description": "（可选）事后回填**实际发生的结果**——校准闭环必需。\
                        ⚠️ `equilibrium_realized` 必须是**预测候选之一**：若实际发生的情况\
                        不在候选里，说明建模漏掉了它——那是**独立信号**（该记录为\
                        「预测未覆盖真相」，不参与对数损失），不是硬塞进去的理由。",
                    "required": ["date", "what_happened", "equilibrium_realized"],
                    "additionalProperties": false,
                    "properties": {
                        "date": {"type": "string", "description": "事件发生日期 YYYY-MM-DD"},
                        "what_happened": {"type": "string", "description": "实际发生了什么（客观陈述）"},
                        "equilibrium_realized": {
                            "type": "string",
                            "description": "实际实现的候选标识（须与预测候选一致）"
                        }
                    }
                }
            }
        }),

        // ── 报告 ──
        "report_render" => json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "case_id": {"type": "string"}
            }
        }),

        // ── 建议包 ──
        "recommend_build" => json!({
            "type": "object",
            "description": "把本阶段的结论汇总为**建议包**（recommend 阶段的交付物）。\
                            调用前应已完成 `sensitivity_scan`（翻转阈值）与 `preference_inject`\
                            （偏好）——它们的结果会被自动收进建议包。",
            "additionalProperties": false,
            "properties": {
                "headline": {
                    "type": "string",
                    "description": "一句话结论（进老板页首段）"
                },
                "constraint_revision":{"type":"integer","minimum":0,"description":"有真实用户硬上限时必填constraint_check返回的最新版本；过期版本不得生成推荐。"},
                "actions": {
                    "type": "array",
                    "description": "建议的行动清单（按优先级）",
                    "items": {
                        "type": "object",
                        "required": ["action"],
                        "properties": {
                            "action": {"type": "string"},
                            "scenario_id":{"type":"string","description":"存在用户硬上限时必须绑定实际已计算且eligible的方案。引擎依据方案属性生成执行金额，文字action/rationale不能覆盖约束。"},
                            "rationale": {"type": "string", "description": "依据（须指向已算出的结果）"},
                            "confidence": {"enum": ["low", "medium", "high"]}
                        }
                    }
                },
                "do_not": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "明确不建议做的事（含理由）——与建议同等重要"
                }
            }
        }),

        // ── 回退提议与裁决结算 ──
        "request_rewind" => json!({
            "type": "object",
            "description": "模型判断当前结论不可靠、须回到更早阶段补材料时调用。分两步完成（铁律 2）：\
                            ① 首次提议回退时传 `target` 与 `reason`（服务端只挂起提议，不回退；须转达用户等待确认）；\
                            ② 收到真实用户在对话中的明确同意或拒绝后，再次调用本工具传 `decision`（approve/reject）与逐字摘自该条回复的 `user_quote`（引擎强校验可信用户账本，严禁模型自行同意回退）。",
            "additionalProperties": false,
            "properties": {
                "target": {
                    "enum": ["intake", "intel", "model_build", "solve", "select", "consequence", "discover"],
                    "description": "提议回退时必填：建议退回到哪个阶段；结算时可选用于核对。"
                },
                "reason": {
                    "type": "string",
                    "description": "提议回退时必填：为什么要回退、补什么材料——这会被展示给用户作为决策依据"
                },
                "decision": {
                    "enum": ["approve", "reject"],
                    "description": "仅在回退提议已发出且收到真实用户明确裁决后传入：approve=用户同意回退（作废下游产物），reject=用户拒绝回退（留在当前阶段）。"
                },
                "user_quote": {
                    "type": "string",
                    "minLength": 1,
                    "description": "传入 decision 时必填：逐字引用发出回退提议之后真实用户在对话中的同意/拒绝原话（引擎在可信用户账本中核验 actor=user，未命中则拒绝回退）。"
                }
            }
        }),

        // ── 检查点 ──
        "checkpoint" => json!({
            "type": "object",
            "description": "向用户确认本阶段的事实/硬假设/结果（铁律 2）。checkpoint 编号由系统按当前阶段决定（intel⇒cp1、model_build⇒cp2、结果阶段⇒cp3）。首次发起或修订清单时传 `items`；当真实用户在工作台点击核准，或在检查点打开之后于对话中明确回复确认/驳回时，可传 `decision` 与逐字摘自该条最新用户回复的 `user_quote`（引擎强校验可信用户账本中的真实用户消息，严禁模型自行伪造确认）。",
            "additionalProperties": false,
            "properties": {
                "items": {
                    "type": "array",
                    "minItems": 1,
                    "description": "发起或更新确认清单时必填。**硬假设（cp2）的每一项都必须有 source**，否则被拒。",
                    "items": {
                        "type": "object",
                        "required": ["content"],
                        "additionalProperties": false,
                        "properties": {
                            "id": {"type": "string"},
                            "content": {"type": "string", "description": "要确认的内容"},
                            "source": {
                                "type": "string",
                                "description": "依据。cp2 必填——没有来源的数值不得进入模型。"
                            },
                            "proposed_value": {"type": "string", "description": "待确认的取值"}
                        }
                    }
                },
                "decision": {
                    "enum": ["approve", "reject"],
                    "description": "仅在检查点已打开且收到真实用户明确裁决后传入：approve=核准通过，reject=驳回修订。"
                },
                "user_quote": {
                    "type": "string",
                    "minLength": 1,
                    "description": "传入 decision 时必填：逐字引用检查点打开之后真实用户在对话中的确认/修改原话（引擎在可信用户账本中核验 actor=user，未命中则拒绝结算）。"
                },
                "checkpoint_id": {
                    "enum": ["cp1", "cp2", "cp3"],
                    "description": "可选：结算时显式指明核对的检查点编号，须与当前阶段挂起的检查点一致。"
                }
            }
        }),

        _ => return None,
    };
    Some(v)
}

/// 未定义 schema 的工具的回退（自由对象）。
///
/// 保留它是对的：**不是所有工具都值得精确 schema**（例如 `sandbox_exec`
/// 的参数面确实开放）。但调用方应能区分"有 schema"与"没有"——
/// 故 `tool_schema` 返回 `Option`。
pub fn freeform() -> serde_json::Value {
    json!({"type": "object", "properties": {}, "additionalProperties": true})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 已接线的工具**都必须有明确 schema**——否则模型只能猜，
    /// 而"猜"的代价实测是 97 次试错。
    #[test]
    fn all_wired_tools_have_schemas() {
        // 与 `domain_exec::wiring_status` 的已接线集对应
        let wired = [
            "select_task_scope", "complete_task", "begin_followup_task",
            "vn_map", "pars_check", "ask_user", "web_search", "web_fetch",
            "participant_profile_update", "gamespec_build", "gamespec_validate",
            "solve_equilibria", "select_equilibria", "select_fuse", "kb_retrieve",
            "monte_carlo", "sensitivity_scan", "novel_move_filter", "discover_satisfice",
            "case_store_read", "case_store_write", "report_render", "checkpoint",
            "request_rewind", "recommend_build",
        ];
        let missing: Vec<&str> = wired
            .iter()
            .filter(|n| tool_schema(n).is_none())
            .copied()
            .collect();
        assert!(
            missing.is_empty(),
            "以下已接线工具没有 schema（模型只能猜参数）：{missing:?}"
        );
    }

    /// schema 必须是**合法 JSON Schema 的最小形态**：有 type、properties 是对象。
    #[test]
    fn schemas_are_well_formed() {
        for name in ["gamespec_build", "checkpoint", "select_equilibria", "web_search"] {
            let s = tool_schema(name).unwrap();
            assert_eq!(s["type"], json!("object"), "{name}");
            assert!(s.get("properties").is_some(), "{name} 缺 properties");
            assert!(s["properties"].is_object(), "{name} 的 properties 必须是对象");
        }
    }

    #[test]
    fn business_cost_schema_requires_scenario_and_cash_treatment() {
        let schema = tool_schema("business_cost").unwrap();
        assert!(schema["required"].as_array().unwrap().contains(&json!("scenario")));
        let cost = &schema["properties"]["costs"]["items"];
        for field in ["scenario", "treatment", "amount"] {
            assert!(cost["required"].as_array().unwrap().contains(&json!(field)));
        }
        assert_eq!(cost["properties"]["treatment"]["enum"], json!([
            "cash_expense", "refundable_deposit", "opportunity_cost", "noncash_expense", "unknown"
        ]));
        assert!(cost["properties"]["treatment"].get("default").is_none());
        assert_eq!(cost["properties"]["amount"]["type"], json!(["number", "null"]));
        for field in ["available_cash", "monthly_revenue_scenario"] {
            assert_eq!(schema["properties"][field]["type"], "number");
        }
        for field in ["contribution_margin_rate", "average_ticket", "open_days_per_month"] {
            assert_eq!(schema["properties"]["sales"]["properties"][field]["type"], "number");
        }
    }

    #[test]
    fn gamespec_validate_schema_prefers_empty_current_model_requests() {
        let schema = tool_schema("gamespec_validate").unwrap();
        assert!(schema.get("required").is_none(), "an empty request must remain valid");
        assert_eq!(schema["additionalProperties"], false);
        assert!(schema["description"].as_str().unwrap().contains("优先用 {}"));
        let provided = &schema["properties"]["spec"];
        for field in ["game_id", "players", "strategies", "payoffs", "params"] {
            assert!(provided["required"].as_array().unwrap().contains(&json!(field)));
        }
        assert_eq!(provided["properties"]["players"]["items"]["type"], "object");
        assert!(provided["properties"]["payoffs"]["required"].as_array().unwrap().contains(&json!("form")));
        assert!(provided["properties"]["payoffs"]["properties"]["values"]["required"].as_array().unwrap().contains(&json!("cells")));
    }

    /// `gamespec_build` 的 schema 必须列出**全部必填字段**——
    /// 实测模型因缺 `params` 而反复失败（它以为参数被环境吞掉）。
    #[test]
    fn gamespec_schema_requires_all_top_level_fields() {
        let s = tool_schema("gamespec_build").unwrap();
        let req: Vec<&str> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        for f in ["game_id", "players", "strategies", "payoffs", "params"] {
            assert!(req.contains(&f), "`{f}` 必须是必填：{req:?}");
        }
        // players 的项必须要求 name（实测最常见的失败）
        let p_req: Vec<&str> = s["properties"]["players"]["items"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(p_req.contains(&"name"), "players[].name 必须必填：{p_req:?}");
    }

    /// 枚举必须用**英文标识**（与 Rust 侧的 serde 值一致）。
    /// 实测模型传了「我方」「主要威胁」这类中文，而列举英文能显著减少这种偏差。
    #[test]
    fn enums_use_english_identifiers() {
        for (tool, path) in [
            ("gamespec_build", "properties.players.items.properties.vn_role"),
            ("gamespec_build", "properties.players.items.properties.relation_to_game"),
            ("select_equilibria", "properties.selector"),
            ("participant_profile_update", "properties.timeline.items.properties.event_type"),
        ] {
            let s = tool_schema(tool).unwrap();
            let mut cur = &s;
            for seg in path.split('.') {
                cur = &cur[seg];
            }
            let vals = cur["enum"].as_array().expect("应有 enum");
            for v in vals {
                let sv = v.as_str().unwrap();
                assert!(
                    sv.is_ascii(),
                    "{tool}.{path} 的枚举值 `{sv}` 不是英文标识——中文取值会让模型猜命名"
                );
            }
        }
    }

    /// 未列出的工具返回 `None`（而不是伪造一个 schema）。
    #[test]
    fn unknown_tool_has_no_schema() {
        assert!(tool_schema("sandbox_exec").is_none());
        assert!(tool_schema("不存在的工具").is_none());
    }
}
