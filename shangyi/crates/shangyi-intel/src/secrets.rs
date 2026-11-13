//! 运行时凭据定位：仅读环境变量与本机受控文件，**绝不执行 shell、绝不回显明文**。
//!
//! 下沉至 `shangyi-intel` 使 CLI 与 MCP 服务端共享同一套凭据发现链，
//! 无需反向依赖 `shangyi-cli`。

use std::path::Path;

/// 从环境变量 `<PROVIDER>_API_KEY` 或 `~/.shangyi-secrets/<provider>.env` 读取 API key。
///
/// 解析刻意**只认整行 `export K=V` / `K=V`**，不执行文件内容——
/// 用 shell 去 source 可能被篡改的文件是提权路径。
pub fn load_api_key(provider: &str) -> Option<String> {
    let var = format!("{}_API_KEY", provider.to_uppercase());
    if let Ok(k) = std::env::var(&var) {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    let home = std::env::var("HOME").ok()?;
    let path = Path::new(&home)
        .join(".shangyi-secrets")
        .join(format!("{provider}.env"));
    read_env_var_from_file(&path, &var)
}

/// 高德 key：先认 `AMAP_KEY`（与 sidecar 的 `amap.env` 同名），
/// 再读 `~/.shangyi-secrets/amap.env` 中的 `AMAP_KEY`，最后回退 `load_api_key("amap")`。
pub fn load_amap_key() -> Option<String> {
    if let Ok(key) = std::env::var("AMAP_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Some(key);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let path = Path::new(&home).join(".shangyi-secrets").join("amap.env");
        if let Some(val) = read_env_var_from_file(&path, "AMAP_KEY") {
            return Some(val);
        }
    }
    load_api_key("amap")
}

/// 本机 Magpie Gateway 调用密钥：
/// 1. 环境变量 `MAGPIE_GATEWAY_KEY`
/// 2. `MAGPIE_API_KEY` 或 `~/.shangyi-secrets/magpie.env`
/// 3. `~/.config/magpie/caller-keys.json`（只取首条非空 `secret`，优先匹配名称含 `dsh`/`shangyi`/`agy` 的条目）
pub fn load_magpie_gateway_key() -> Option<String> {
    if let Ok(k) = std::env::var("MAGPIE_GATEWAY_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    if let Some(k) = load_api_key("magpie") {
        return Some(k);
    }
    let home = std::env::var("HOME").ok()?;
    let path = Path::new(&home).join(".config/magpie/caller-keys.json");
    parse_magpie_caller_keys_file(&path)
}

/// 从 `caller-keys.json` 解析可用调用密钥（纯文件解析，便于在无 `$HOME` 污染下单测）。
pub fn parse_magpie_caller_keys_json(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let entries = value.as_array()?;
    let mut fallback: Option<String> = None;
    for item in entries {
        if item.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
            continue;
        }
        let Some(secret) = item
            .get("secret")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let name = item
            .get("name")
            .or_else(|| item.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if name.contains("shangyi") || name.contains("dsh") || name.contains("agy") {
            return Some(secret.to_string());
        }
        if fallback.is_none() {
            fallback = Some(secret.to_string());
        }
    }
    fallback
}

fn parse_magpie_caller_keys_file(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_magpie_caller_keys_json(&text)
}

/// 从 `.env` 风格文件安全读取指定键的值（不执行任何命令）。
pub fn read_env_var_from_file(path: &Path, expected_key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() == expected_key {
            let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_caller_keys_preferring_dsh_or_shangyi_entry() {
        let raw = r#"[
            {"id":"other","name":"cli","lan":false,"secret":"mgp_first"},
            {"id":"dsh-local","name":"DSH Caller","lan":false,"secret":"mgp_dsh"}
        ]"#;
        assert_eq!(parse_magpie_caller_keys_json(raw).as_deref(), Some("mgp_dsh"));
    }

    #[test]
    fn skips_disabled_caller_keys_and_falls_back_to_first_valid() {
        let raw = r#"[
            {"id":"dsh","name":"dsh","enabled":false,"secret":"mgp_disabled"},
            {"id":"fallback","name":"local","secret":"mgp_ok"}
        ]"#;
        assert_eq!(parse_magpie_caller_keys_json(raw).as_deref(), Some("mgp_ok"));
    }
}
