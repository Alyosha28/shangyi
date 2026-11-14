//! `shangyi start` / `shangyi ui` 单一推荐新手入口：
//!
//! 1. 初始化持久化工作区 `$SHANGYI_HOME`（默认 `~/.shangyi/cases`）；
//! 2. 解析当前正在执行的 `shangyi` 二进制作为 `SHANGYI_BIN`，确保 `mcp-shangyi` 39 项确定性工具不走丢；
//! 3. 检查并同步 DSH `shangyi-web` Profile 与三栏工作台插件；
//! 4. 拉起 `dsh --profile shangyi-web`（默认端口 8791），自动打开工作台或输出访问地址。

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::run::{RunFailure, shangyi_home};

pub fn ensure_persistent_workspace() -> Result<PathBuf, RunFailure> {
    let home = shangyi_home()?;
    for sub in ["cases", "runtime", "kb"] {
        let dir = home.join(sub);
        std::fs::create_dir_all(&dir).map_err(|e| RunFailure::CaseDir {
            path: dir.display().to_string(),
            detail: format!("无法初始化持久化目录 `{sub}`：{e}"),
        })?;
    }
    Ok(home)
}

pub fn locate_repo_root() -> Option<PathBuf> {
    let is_repo = |p: &Path| {
        p.join("dsh/profile-web/cordis.patch.yml").is_file()
            && p.join("shangyi/ui/scripts/install-workbench.sh").is_file()
    };

    if let Ok(explicit) = std::env::var("SHANGYI_REPO_ROOT") {
        let p = PathBuf::from(explicit.trim());
        if is_repo(&p) {
            return Some(p);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        for anc in cwd.ancestors() {
            if is_repo(anc) {
                return Some(anc.to_path_buf());
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        for anc in exe.ancestors() {
            if is_repo(anc) {
                return Some(anc.to_path_buf());
            }
        }
    }
    let compiled = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    if is_repo(&compiled) {
        return std::fs::canonicalize(&compiled).ok().or(Some(compiled));
    }
    None
}

fn check_dsh_available() -> Result<(), RunFailure> {
    match Command::new("dsh").arg("--version").output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(RunFailure::Session(format!(
            "检测到 `dsh` 命令但执行 `--version` 失败（退出码 {:?}）。请确认 DeepSeek Harness 环境已正确安装。",
            out.status.code()
        ))),
        Err(e) => Err(RunFailure::Session(format!(
            "PATH 中未找到 `dsh`（DeepSeek Harness 宿主）：{e}。\n\
             请先安装 DSH CLI，或确认包含 `dsh` 的 Node bin 目录已加入 PATH。"
        ))),
    }
}

pub fn run_start(port: Option<u16>, no_open: bool) -> Result<(), RunFailure> {
    let home = ensure_persistent_workspace()?;
    let port = port.unwrap_or(8791);
    check_dsh_available()?;

    let self_bin = std::env::current_exe().map_err(|e| {
        RunFailure::Session(format!("无法定位当前 `shangyi` 可执行文件路径：{e}"))
    })?;
    let dsh_home = std::env::var("DSH_HOME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".dsh")))
        .ok_or_else(|| RunFailure::Session("未设置 HOME 或 DSH_HOME，无法定位 DSH profile 目录".into()))?;

    if let Some(repo_root) = locate_repo_root() {
        let installer = repo_root.join("shangyi/ui/scripts/install-workbench.sh");
        println!("正在同步商奕 Web 工作台与插件（{}）...", installer.display());
        let status = Command::new("bash")
            .arg(&installer)
            .arg("--install-only")
            .env("SHANGYI_BIN", &self_bin)
            .env("SHANGYI_HOME", &home)
            .env("DSH_HOME", &dsh_home)
            .status()
            .map_err(|e| RunFailure::Session(format!("执行工作台安装脚本失败：{e}")))?;
        if !status.success() {
            return Err(RunFailure::Session(format!(
                "工作台插件同步失败（退出码 {:?}）。请检查输出日志后重试。",
                status.code()
            )));
        }
    } else if !dsh_home.join("profiles/shangyi-web/cordis.patch.yml").is_file() {
        return Err(RunFailure::Session(format!(
            "既未找到商奕源码仓库，在 `{}` 下也未安装 `shangyi-web` profile。\n\
             请在仓库根目录运行：`bash shangyi/ui/scripts/install-workbench.sh`（或设置 `SHANGYI_REPO_ROOT`）。",
            dsh_home.display()
        )));
    }

    println!();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  商奕（ShangYi）宣纸·墨玉决策工作台正在启动");
    println!("  确定性内核 : {}", self_bin.display());
    println!("  案例持久化 : {}", home.join("cases").display());
    println!("  DSH 配置   : {}/profiles/shangyi-web", dsh_home.display());
    println!("  监听端口   : http://127.0.0.1:{port}");
    println!("  退出提示   : 按 Ctrl+C 结束服务");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    let mut cmd = Command::new("dsh");
    cmd.arg("--profile")
        .arg("shangyi-web")
        .arg("--port")
        .arg(port.to_string())
        .env("SHANGYI_BIN", &self_bin)
        .env("SHANGYI_HOME", &home)
        .env("DSH_HOME", &dsh_home);
    if let Some(repo_root) = locate_repo_root() {
        cmd.env("SHANGYI_ROOT", repo_root);
    }
    if no_open {
        cmd.arg("--no-open");
    }

    let status = cmd
        .status()
        .map_err(|e| RunFailure::Session(format!("启动 `dsh --profile shangyi-web` 失败：{e}")))?;
    if !status.success() {
        return Err(RunFailure::Session(format!(
            "DSH 工作台异常退出（退出码 {:?}）",
            status.code()
        )));
    }
    Ok(())
}
