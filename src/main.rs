use std::time::Duration;

use tracing::info;

use extrema_infra::prelude::*;

use extrema_guard::arch::{
    executor::{GuardExecutor, load_guard_executor_config},
    okx_outside_range_cancel_module::{
        base::OkxOutsideRangeCancel, utils::load_okx_outside_range_cancel_config,
    },
    profit_lock_module::base::ProfitLock,
    profit_lock_module::utils::load_profit_lock_config,
    static_protect_module::{base::StaticProtect, utils::load_static_protect_config},
};

fn schedule_task(duration_sec: u64, task_id: u64) -> AltTaskInfo {
    AltTaskInfo {
        alt_task_type: AltTaskType::TimeScheduler(Duration::from_secs(duration_sec)),
        chunk: 1,
        task_base_id: Some(task_id),
    }
}

#[tokio::main]
async fn main() -> InfraResult<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    tracing_subscriber::fmt::init();

    let executor_config = load_guard_executor_config()?;
    let static_config = load_static_protect_config()?;
    let profit_lock_config = load_profit_lock_config()?;
    let outside_range_config = load_okx_outside_range_cancel_config()?;
    info!(
        mode = ?executor_config.mode,
        exchanges = ?executor_config.exchanges,
        "starting extrema guard"
    );

    let executor = GuardExecutor::connect(&executor_config).await?;
    let static_protect = StaticProtect::new(&static_config, executor.clone());
    let profit_lock = ProfitLock::new(&profit_lock_config, executor.clone());
    let outside_range = OkxOutsideRangeCancel::new(&outside_range_config, executor)?;

    let env = EnvBuilder::new()
        .with_task(schedule_task(
            static_config.schedule_duration_sec,
            static_config.schedule_task_id,
        ))
        .with_task(schedule_task(
            profit_lock_config.schedule_duration_sec,
            profit_lock_config.schedule_task_id,
        ))
        .with_task(schedule_task(
            outside_range_config.schedule_duration_sec,
            outside_range_config.schedule_task_id,
        ))
        .with_strategy_module(static_protect)
        .with_strategy_module(profit_lock)
        .with_strategy_module(outside_range)
        .build()?;

    env.execute().await;

    Ok(())
}
