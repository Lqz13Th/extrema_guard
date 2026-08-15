//! Copy this file into your own binary crate and depend on `extrema_guard`.
//! The custom module owns its config loader; the binary composes it with the
//! built-ins and the single shared executor.

use std::{env::current_dir, fs, sync::Arc, time::Duration};

use serde::Deserialize;
use tracing::info;

use extrema_infra::prelude::*;

use extrema_guard::arch::{
    executor::{GuardExecutor, load_guard_executor_config},
    okx_outside_range_cancel_module::{
        base::OkxOutsideRangeCancel, utils::load_okx_outside_range_cancel_config,
    },
    profit_lock_module::{base::ProfitLock, utils::load_profit_lock_config},
    static_protect_module::{base::StaticProtect, utils::load_static_protect_config},
};

#[derive(Clone, Debug, Deserialize)]
struct MyCustomExitConfig {
    schedule_duration_sec: u64,
    schedule_task_id: u64,
}

#[derive(Deserialize)]
struct StrategyConfigToml {
    my_custom_exit: MyCustomExitConfig,
}

fn load_my_custom_exit_config() -> InfraResult<MyCustomExitConfig> {
    let path = current_dir()
        .map_err(InfraError::Io)?
        .join("strategy_config.toml");
    let raw = fs::read_to_string(&path)
        .map_err(|err| InfraError::Msg(format!("read {}: {err}", path.display())))?;
    let wrapper: StrategyConfigToml = toml::from_str(&raw)
        .map_err(|err| InfraError::Msg(format!("parse {}: {err}", path.display())))?;
    Ok(wrapper.my_custom_exit)
}

fn schedule_task(duration_sec: u64, task_id: u64) -> AltTaskInfo {
    AltTaskInfo {
        alt_task_type: AltTaskType::TimeScheduler(Duration::from_secs(duration_sec)),
        chunk: 1,
        task_base_id: Some(task_id),
    }
}

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

    let executor_config = load_guard_executor_config()?;
    let static_config = load_static_protect_config()?;
    let profit_lock_config = load_profit_lock_config()?;
    let outside_range_config = load_okx_outside_range_cancel_config()?;
    let custom_config = load_my_custom_exit_config()?;
    let executor = GuardExecutor::connect(&executor_config).await?;

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
        .with_task(schedule_task(
            custom_config.schedule_duration_sec,
            custom_config.schedule_task_id,
        ))
        .with_strategy_module(StaticProtect::new(&static_config, executor.clone()))
        .with_strategy_module(ProfitLock::new(&profit_lock_config, executor.clone()))
        .with_strategy_module(OkxOutsideRangeCancel::new(
            &outside_range_config,
            executor.clone(),
        )?)
        .with_strategy_module(MyCustomExit::new(custom_config.schedule_task_id, executor))
        .build()?;

    env.execute().await;

    Ok(())
}
