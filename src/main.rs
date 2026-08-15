use std::time::Duration;

use tracing::info;

use extrema_infra::prelude::*;

use extrema_guard::arch::{
    common::{config::load_guard_config, executor::GuardExecutor},
    profit_lock_module::base::ProfitLock,
    static_protect_module::base::StaticProtect,
};

#[tokio::main]
async fn main() -> InfraResult<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    tracing_subscriber::fmt::init();

    let config = load_guard_config()?;
    info!(
        mode = ?config.guard.mode,
        exchanges = ?config.guard.exchanges,
        "starting extrema guard"
    );

    let executor = GuardExecutor::connect(&config).await?;

    let scheduler_task = AltTaskInfo {
        alt_task_type: AltTaskType::TimeScheduler(Duration::from_secs(config.guard.poll_seconds)),
        chunk: 1,
        task_base_id: Some(config.guard.schedule_task_id),
    };

    let static_protect = StaticProtect::new(
        &config.static_protect,
        config.guard.schedule_task_id,
        executor.clone(),
    );
    let profit_lock = ProfitLock::new(&config.profit_lock, config.guard.schedule_task_id, executor);

    let env = EnvBuilder::new()
        .with_task(scheduler_task)
        .with_strategy_module(static_protect)
        .with_strategy_module(profit_lock)
        .build()?;

    env.execute().await;

    Ok(())
}
