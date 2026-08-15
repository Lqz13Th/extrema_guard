use std::sync::Arc;

use tracing::{error, info};

use extrema_infra::prelude::*;

use super::base::OkxOutsideRangeCancel;

impl Strategy for OkxOutsideRangeCancel {
    async fn initialize(&mut self) {
        if self.config.enabled {
            info!(
                outside_seconds = self.config.outside_seconds,
                "initializing OKX outside-range cancel"
            );
        }
    }

    fn strategy_name(&self) -> &'static str {
        "OkxOutsideRangeCancel"
    }
}

impl CommandEmitter for OkxOutsideRangeCancel {
    fn command_init(&mut self, command_registry: Arc<CommandRegistry>) {
        self.command_registry = command_registry;
    }

    fn command_registry(&self) -> Arc<CommandRegistry> {
        self.command_registry.clone()
    }
}

impl EventHandler for OkxOutsideRangeCancel {
    async fn on_schedule(&mut self, msg: InfraMsg<AltScheduleEvent>) {
        if !self.config.enabled || msg.task_id != self.config.schedule_task_id {
            return;
        }
        if let Err(err) = self.reconcile().await {
            error!(error = ?err, "outside-range cancel reconcile failed");
        }
    }
}
