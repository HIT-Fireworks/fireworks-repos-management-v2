use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use repository_tui::{run, run_headless};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let mut workspace = PathBuf::from(".");
    let mut check = false;
    while let Some(argument) = args.next() {
        match argument.to_string_lossy().as_ref() {
            "--workspace" | "--root" => {
                workspace = PathBuf::from(args.next().context("--workspace 缺少路径")?);
            }
            "--check" => check = true,
            "-h" | "--help" => {
                println!(
                    "repository-tui [--workspace PATH] [--check]\n\n\
                     --workspace PATH  指定包含 v4 manifest/topology/routes 的工作区\n\
                     --check           通过 JSON 核心加载状态并输出摘要"
                );
                return Ok(());
            }
            unknown => bail!("未知参数：{unknown}"),
        }
    }
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
