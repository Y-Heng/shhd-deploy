//! Docker 部署：SSH 到 Linux 服务器，在工作目录按顺序执行配置的命令。

use crate::config::AppConfig;
use crate::events::TaskLogger;
use crate::ssh;
use anyhow::{bail, Context, Result};
use tokio_util::sync::CancellationToken;

/// Docker 部署：SSH 到目标 Linux 服务器按顺序执行命令
pub async fn run_docker_deploy(
    config: AppConfig,
    target_id: String,
    logger: TaskLogger,
    cancel: CancellationToken,
) -> Result<()> {
    let target = config
        .docker_targets
        .iter()
        .find(|candidate| candidate.id == target_id)
        .with_context(|| format!("找不到 Docker 部署目标: {}", target_id))?
        .clone();

    let server = config.find_server(&target.server_id)?.clone();
    logger.info(format!("连接服务器 {} ...", server.name));
    let conn = ssh::connect(&config, &server.id).await?;
    logger.success(format!("已连接 {}", server.name));

    let total_commands = target.commands.len().max(1);
    for (index, command) in target.commands.iter().enumerate() {
        if cancel.is_cancelled() {
            bail!("任务已被取消");
        }
        logger.progress(
            (index as f64 / total_commands as f64) * 100.0,
            command.clone(),
        );
        logger.info(format!("$ {}", command));

        let escaped_dir = target.work_dir.replace('\'', "'\\''");
        let full_command =
            ssh::shell_command(&format!("cd '{}' && {}", escaped_dir, command));
        let mut line_callback = |line: &str| {
            if !line.trim().is_empty() {
                logger.info(format!("  {}", line));
            }
        };
        let output = ssh::exec(&conn, &full_command, Some(&mut line_callback)).await?;
        if !output.success() {
            bail!(
                "命令执行失败(退出码 {}): {}",
                output.exit_code,
                output.stderr.chars().take(2000).collect::<String>()
            );
        }
    }

    logger.progress(100.0, "完成");
    logger.success(format!("Docker 部署目标 {} 执行完成", target.name));
    Ok(())
}
