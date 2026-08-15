use std::sync::Arc;

use tracing::{error, info};

use extrema_infra::prelude::*;

use super::base::ProfitLock;

impl Strategy for ProfitLock {
    async fn initialize(&mut self) {
        if !self.enabled || self.rules.is_empty() {
            info!("profit lock has no rules configured; module stays inert");
            return;
        }
        info!(rules = self.rules.len(), "initializing profit lock");
    }

    fn strategy_name(&self) -> &'static str {
        "ProfitLock"
    }
}

impl CommandEmitter for ProfitLock {
    fn command_init(&mut self, command_registry: Arc<CommandRegistry>) {
        self.command_registry = command_registry;
    }

    fn command_registry(&self) -> Arc<CommandRegistry> {
        self.command_registry.clone()
    }
}

impl EventHandler for ProfitLock {
    async fn on_schedule(&mut self, msg: InfraMsg<AltScheduleEvent>) {
        if !self.enabled || msg.task_id != self.schedule_task_id || self.rules.is_empty() {
            return;
        }
        if let Err(err) = self.reconcile().await {
            error!(error = ?err, "profit lock reconcile failed");
        }
    }
}
