use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use tracing::{error, info};

use extrema_infra::prelude::*;

use crate::arch::executor::{GuardExecutor, position_key};

use super::utils::{ProfitLockConfig, ProfitLockRule, armed, lock_price};

const REFIRE_AFTER: Duration = Duration::from_secs(30);

/// Strategy 2: once price has moved `activation_pct` past entry, keep a
/// protective exit armed at `lock_pct` past entry. The armed flag derives
/// from `water`, an in-memory per-position best-price mark scoped to the
/// position lifecycle: it survives pullbacks while the process lives and
/// re-accumulates from the current mark after a restart.
#[derive(Clone)]
pub struct ProfitLock {
    pub enabled: bool,
    pub rules: Vec<ProfitLockRule>,
    pub schedule_task_id: u64,
    pub executor: GuardExecutor,
    pub command_registry: Arc<CommandRegistry>,
    water: HashMap<String, f64>,
    fired: HashMap<String, Instant>,
}

impl ProfitLock {
    pub fn new(config: &ProfitLockConfig, executor: GuardExecutor) -> Self {
        Self {
            enabled: config.enabled,
            rules: config.rules.clone(),
            schedule_task_id: config.schedule_task_id,
            executor,
            command_registry: Arc::default(),
            water: HashMap::new(),
            fired: HashMap::new(),
        }
    }

    pub(crate) async fn reconcile(&mut self) -> InfraResult<()> {
        let positions = self.executor.positions().await?;
        let mut live_keys = HashSet::new();

        for position in &positions {
            let Some(rule) = match_rule(&self.rules, &position.inst) else {
                continue;
            };
            let key = position_key(&position.exchange, &position.inst);
            live_keys.insert(key.clone());

            let mark = position.mark_price;
            let water = {
                let entry = self.water.entry(key.clone()).or_insert(mark);
                if position.is_long() {
                    if mark > *entry {
                        *entry = mark;
                    }
                } else if mark < *entry {
                    *entry = mark;
                }
                *entry
            };

            if !armed(
                position.avg_price,
                water,
                rule.activation_pct,
                position.is_long(),
            ) {
                continue;
            }
            let lock = lock_price(position.avg_price, rule.lock_pct, position.is_long());
            let crossed = if position.is_long() {
                mark <= lock
            } else {
                mark >= lock
            };
            if !crossed || !self.may_fire(&key) {
                continue;
            }
            info!(
                exchange = position.exchange.as_str(),
                inst = position.inst.as_str(),
                mark,
                lock,
                water,
                "profit lock crossed; closing position"
            );
            match self.executor.close_pct(position, 100.0).await {
                Ok(()) => {
                    self.fired.insert(key, Instant::now());
                },
                Err(err) => error!(error = ?err, "profit lock close failed; retried next round"),
            }
        }

        self.water.retain(|key, _| live_keys.contains(key));
        self.fired.retain(|key, _| live_keys.contains(key));
        Ok(())
    }

    fn may_fire(&self, key: &str) -> bool {
        !self
            .fired
            .get(key)
            .is_some_and(|at| at.elapsed() < REFIRE_AFTER)
    }
}

fn match_rule(rules: &[ProfitLockRule], inst: &str) -> Option<ProfitLockRule> {
    rules
        .iter()
        .find(|rule| rule.inst == inst)
        .or_else(|| rules.iter().find(|rule| rule.inst == "*"))
        .cloned()
}
