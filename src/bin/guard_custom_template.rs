//! Copy this file to add your own guard module next to the built-ins.
//! Replace `MyCustomExit` with your logic; everything else is the same
//! assembly as `src/main.rs` plus one extra `with_strategy_module` line.

use std::{sync::Arc, time::Duration};

use tracing::info;

use extrema_infra::prelude::*;

use extrema_guard::arch::{
    common::{config::load_guard_config, executor::GuardExecutor},
    profit_lock_module::base::ProfitLock,
    static_protect_module::base::StaticProtect,
};

#[derive(Clone)]
struct MyCustomExit {
    schedule_task_id: u64,
    executor: GuardExecutor,
    command_registry: Arc<CommandRegistry>,
}

impl MyCustomExit {
    fn new(schedule_task_id: u64, executor: GuardExecutor) -> Self {
        Self {
            schedule_task_id,
            executor,
            command_registry: Arc::default(),
        }
    }
}

impl Strategy for MyCustomExit {
    async fn initialize(&mut self) {
        info!("initializing my custom exit");
    }

    fn strategy_name(&self) -> &'static str {
        "MyCustomExit"
    }
}

impl CommandEmitter for MyCustomExit {
    fn command_init(&mut self, command_registry: Arc<CommandRegistry>) {
        self.command_registry = command_registry;
    }

    fn command_registry(&self) -> Arc<CommandRegistry> {
        self.command_registry.clone()
    }
}

impl EventHandler for MyCustomExit {
    async fn on_schedule(&mut self, msg: InfraMsg<AltScheduleEvent>) {
        if msg.task_id != self.schedule_task_id {
            return;
        }
        let _ = self.executor.is_dry_run();
    }
}

#[tokio::main]
async fn main() -> InfraResult<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    tracing_subscriber::fmt::init();

    let config = load_guard_config()?;
    let executor = GuardExecutor::connect(&config).await?;

    let scheduler_task = AltTaskInfo {
        alt_task_type: AltTaskType::TimeScheduler(Duration::from_secs(config.guard.poll_seconds)),
        chunk: 1,
        task_base_id: Some(config.guard.schedule_task_id),
    };

    let env = EnvBuilder::new()
        .with_task(scheduler_task)
        .with_strategy_module(StaticProtect::new(
            &config.static_protect,
            config.guard.schedule_task_id,
            executor.clone(),
        ))
        .with_strategy_module(ProfitLock::new(
            &config.profit_lock,
            config.guard.schedule_task_id,
            executor.clone(),
        ))
        .with_strategy_module(MyCustomExit::new(config.guard.schedule_task_id, executor))
        .build()?;

    env.execute().await;

    Ok(())
}
