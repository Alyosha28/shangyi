//! 用**实测采集的真实 spec** 复现 solve 阶段的溢出（run12 崩溃）。
//!
//! spec 来自一次真实咨询的 gamespec_build 调用（38 参数、3 玩家、8 组合）。
//! 它在 solve 阶段让 `Rational` 的 i128 溢出并 **panic**——库代码 panic
//! 会拖垮整个进程（CLI 没有机会把它变成可读错误）。

use shangyi_core::agent::{ExecOutcome, ToolCall, ToolExecutor};
use shangyi_core::domain_exec::DomainExecutor;
use shangyi_core::phase_graph::SessionPhase;
use shangyi_kb::TheoryLibrary;
use shangyi_playbooks::{Playbook, PlaybookMeta};

const REAL_SPEC: &str = r#"
{
  "game_id": "qinghe_fresh_price_cut_repeated",
  "params": {
    "breakeven_mult": {
      "name": "breakeven_mult",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "multiple",
      "value": {
        "kind": "literal",
        "value": 1.790123
      }
    },
    "c": {
      "name": "c",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/unit",
      "value": {
        "kind": "literal",
        "value": 9.9
      }
    },
    "cut": {
      "name": "cut",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "ratio",
      "value": {
        "kind": "literal",
        "value": 0.1
      }
    },
    "delta": {
      "name": "delta",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "dimensionless",
      "value": {
        "kind": "literal",
        "value": 0.99
      }
    },
    "eps": {
      "name": "eps",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "dimensionless",
      "value": {
        "kind": "literal",
        "value": 6.0
      }
    },
    "eps_star": {
      "name": "eps_star",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "dimensionless",
      "value": {
        "kind": "literal",
        "value": 9.132
      }
    },
    "m_cut": {
      "name": "m_cut",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/unit",
      "value": {
        "kind": "literal",
        "value": 1.62
      }
    },
    "m_hold": {
      "name": "m_hold",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/unit",
      "value": {
        "kind": "literal",
        "value": 2.9
      }
    },
    "margin_floor": {
      "name": "margin_floor",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "ratio",
      "value": {
        "kind": "literal",
        "value": 0.18
      }
    },
    "p0": {
      "name": "p0",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/unit",
      "value": {
        "kind": "literal",
        "value": 12.8
      }
    },
    "p_min_floor": {
      "name": "p_min_floor",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/unit",
      "value": {
        "kind": "literal",
        "value": 12.073171
      }
    },
    "p_promo": {
      "name": "p_promo",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/unit",
      "value": {
        "kind": "literal",
        "value": 11.52
      }
    },
    "u_HHH_qinghe": {
      "name": "u_HHH_qinghe",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.828571
      }
    },
    "u_HHH_rival_a": {
      "name": "u_HHH_rival_a",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 1.242857
      }
    },
    "u_HHH_rival_b": {
      "name": "u_HHH_rival_b",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.828571
      }
    },
    "u_HHL_qinghe": {
      "name": "u_HHL_qinghe",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.66185
      }
    },
    "u_HHL_rival_a": {
      "name": "u_HHL_rival_a",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.992777
      }
    },
    "u_HHL_rival_b": {
      "name": "u_HHL_rival_b",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.695699
      }
    },
    "u_HLH_qinghe": {
      "name": "u_HLH_qinghe",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.601344
      }
    },
    "u_HLH_rival_a": {
      "name": "u_HLH_rival_a",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.948157
      }
    },
    "u_HLH_rival_b": {
      "name": "u_HLH_rival_b",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.601344
      }
    },
    "u_HLL_qinghe": {
      "name": "u_HLL_qinghe",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.508408
      }
    },
    "u_HLL_rival_a": {
      "name": "u_HLL_rival_a",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.80161
      }
    },
    "u_HLL_rival_b": {
      "name": "u_HLL_rival_b",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.534401
      }
    },
    "u_LHH_qinghe": {
      "name": "u_LHH_qinghe",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.695699
      }
    },
    "u_LHH_rival_a": {
      "name": "u_LHH_rival_a",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.992777
      }
    },
    "u_LHH_rival_b": {
      "name": "u_LHH_rival_b",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.66185
      }
    },
    "u_LHL_qinghe": {
      "name": "u_LHL_qinghe",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.579158
      }
    },
    "u_LHL_rival_a": {
      "name": "u_LHL_rival_a",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.826457
      }
    },
    "u_LHL_rival_b": {
      "name": "u_LHL_rival_b",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.579158
      }
    },
    "u_LLH_qinghe": {
      "name": "u_LLH_qinghe",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.534401
      }
    },
    "u_LLH_rival_a": {
      "name": "u_LLH_rival_a",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.80161
      }
    },
    "u_LLH_rival_b": {
      "name": "u_LLH_rival_b",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.508408
      }
    },
    "u_LLL_qinghe": {
      "name": "u_LLL_qinghe",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.462857
      }
    },
    "u_LLL_rival_a": {
      "name": "u_LLL_rival_a",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.694286
      }
    },
    "u_LLL_rival_b": {
      "name": "u_LLL_rival_b",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "CNY/market-unit",
      "value": {
        "kind": "literal",
        "value": 0.462857
      }
    },
    "w_rival_a": {
      "name": "w_rival_a",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "weight",
      "value": {
        "kind": "literal",
        "value": 1.5
      }
    },
    "w_rival_b": {
      "name": "w_rival_b",
      "source": {
        "checkpoint": "cp2",
        "kind": "user_confirmed"
      },
      "unit": "weight",
      "value": {
        "kind": "literal",
        "value": 1.0
      }
    }
  },
  "payoffs": {
    "form": "normal",
    "values": {
      "cells": {
        "维持(12.8),维持(12.8),维持(12.8)": {
          "qinghe": {
            "kind": "ref",
            "value": "u_HHH_qinghe"
          },
          "rival_a": {
            "kind": "ref",
            "value": "u_HHH_rival_a"
          },
          "rival_b": {
            "kind": "ref",
            "value": "u_HHH_rival_b"
          }
        },
        "维持(12.8),维持(12.8),降价(11.52)": {
          "qinghe": {
            "kind": "ref",
            "value": "u_HHL_qinghe"
          },
          "rival_a": {
            "kind": "ref",
            "value": "u_HHL_rival_a"
          },
          "rival_b": {
            "kind": "ref",
            "value": "u_HHL_rival_b"
          }
        },
        "维持(12.8),降价(11.52),维持(12.8)": {
          "qinghe": {
            "kind": "ref",
            "value": "u_HLH_qinghe"
          },
          "rival_a": {
            "kind": "ref",
            "value": "u_HLH_rival_a"
          },
          "rival_b": {
            "kind": "ref",
            "value": "u_HLH_rival_b"
          }
        },
        "维持(12.8),降价(11.52),降价(11.52)": {
          "qinghe": {
            "kind": "ref",
            "value": "u_HLL_qinghe"
          },
          "rival_a": {
            "kind": "ref",
            "value": "u_HLL_rival_a"
          },
          "rival_b": {
            "kind": "ref",
            "value": "u_HLL_rival_b"
          }
        },
        "降价(11.52),维持(12.8),维持(12.8)": {
          "qinghe": {
            "kind": "ref",
            "value": "u_LHH_qinghe"
          },
          "rival_a": {
            "kind": "ref",
            "value": "u_LHH_rival_a"
          },
          "rival_b": {
            "kind": "ref",
            "value": "u_LHH_rival_b"
          }
        },
        "降价(11.52),维持(12.8),降价(11.52)": {
          "qinghe": {
            "kind": "ref",
            "value": "u_LHL_qinghe"
          },
          "rival_a": {
            "kind": "ref",
            "value": "u_LHL_rival_a"
          },
          "rival_b": {
            "kind": "ref",
            "value": "u_LHL_rival_b"
          }
        },
        "降价(11.52),降价(11.52),维持(12.8)": {
          "qinghe": {
            "kind": "ref",
            "value": "u_LLH_qinghe"
          },
          "rival_a": {
            "kind": "ref",
            "value": "u_LLH_rival_a"
          },
          "rival_b": {
            "kind": "ref",
            "value": "u_LLH_rival_b"
          }
        },
        "降价(11.52),降价(11.52),降价(11.52)": {
          "qinghe": {
            "kind": "ref",
            "value": "u_LLL_qinghe"
          },
          "rival_a": {
            "kind": "ref",
            "value": "u_LLL_rival_a"
          },
          "rival_b": {
            "kind": "ref",
            "value": "u_LLL_rival_b"
          }
        }
      }
    }
  },
  "players": [
    {
      "id": "qinghe",
      "name": "青禾（社区生鲜）",
      "relation_to_game": "player",
      "vn_role": "focal"
    },
    {
      "id": "rival_a",
      "name": "连锁 A",
      "relation_to_game": "player",
      "vn_role": "substitutor"
    },
    {
      "id": "rival_b",
      "name": "连锁 B",
      "relation_to_game": "player",
      "vn_role": "substitutor"
    }
  ],
  "strategies": {
    "qinghe": [
      "维持(12.8)",
      "降价(11.52)"
    ],
    "rival_a": [
      "维持(12.8)",
      "降价(11.52)"
    ],
    "rival_b": [
      "维持(12.8)",
      "降价(11.52)"
    ]
  }
}
"#;

fn the_playbook() -> Playbook {
    Playbook {
        meta: PlaybookMeta { id: "p".into(), name: "n".into(), description: String::new(), order: 0, version: 1 },
        applicable_when: vec![], fails_when: vec![], required_inputs: vec![],
        value_net: None, intake: vec![], models: vec![], selectors: Default::default(),
        tool_allowlist: vec![], report_vars: Default::default(),
    }
}

#[test]
fn real_spec_solves_without_panicking() {
    let theory = TheoryLibrary::new();
    let pb = the_playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let spec: serde_json::Value = serde_json::from_str(REAL_SPEC).expect("spec 应可解析");

    let out = ex.execute(
        &ToolCall { call_id: "c1".into(), name: "gamespec_build".into(), arguments: spec },
        SessionPhase::ModelBuild,
    );
    assert!(matches!(out, ExecOutcome::Ok(_)), "构建应成功：{out:?}");

    // 求解：**不得 panic**。允许类型化失败（退化/超范围），但进程必须活着。
    let out2 = ex.execute(
        &ToolCall { call_id: "c2".into(), name: "solve_equilibria".into(), arguments: serde_json::json!({}) },
        SessionPhase::Solve,
    );
    match &out2 {
        ExecOutcome::Ok(s) => println!("  求解成功：{}", &s[..s.len().min(120)]),
        ExecOutcome::Failed(m) => println!("  类型化失败（可接受）：{}", &m[..m.len().min(200)]),
        other => println!("  其它：{other:?}"),
    }
}
