use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use repository_tui::{run, run_headless, Manager};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let mut workspace: Option<PathBuf> = None;
    let mut check = false;
    while let Some(argument) = args.next() {
        match argument.to_string_lossy().as_ref() {
            "--workspace" | "--root" => {
                workspace = Some(PathBuf::from(args.next().context("--workspace 缺少路径")?));
            }
            "--check" => check = true,
            "-h" | "--help" => {
                println!(
                    "薪火资料管理 [--workspace PATH] [--check]\n\n\
                     普通使用请直接双击启动，无需参数。\n\
                     --workspace PATH  仅供维护人员指定数据目录\n\
                     --check           检查安装包是否完整"
                );
                return Ok(());
            }
            unknown => bail!("未知参数：{unknown}"),
        }
    }
    let workspace = match workspace {
        Some(path) => path,
        None => Manager::discover()?.workspace().to_path_buf(),
    };
    if check {
        let dashboard = run_headless(workspace)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "health": dashboard.health,
                "repository_count": dashboard.repositories.len(),
                "file_route_total": dashboard.routes.file_total,
                "file_routes_loaded": dashboard.routes.file_routes.len(),
                "course_code_route_total": dashboard.routes.course_code_total,
                "course_code_routes_loaded": dashboard.routes.course_code_routes.len(),
                "plan_count": dashboard.plans.len(),
                "journal_count": dashboard.journals.len(),
                "query": dashboard.query,
                "logs": dashboard.logs
            }))?
        );
        return Ok(());
    }
    run(workspace)
}
