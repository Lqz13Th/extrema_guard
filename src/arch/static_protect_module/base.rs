use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use tracing::{error, info, warn};

use extrema_infra::prelude::*;

use crate::arch::executor::{GuardExecutor, PositionSnapshot, position_key};

use super::utils::{
    StaticProtectConfig, StaticRule, offset_from_pct, offset_from_risk, stop_price, target_price,
};

const REFIRE_AFTER: Duration = Duration::from_secs(30);

/// Strategy 1: static stop-loss / take-profit derived from entry price, sized
/// by percentage or by quote-currency risk. The take-profit rests on the book
/// as a reduce-only limit; the stop-loss is soft-triggered by this loop.
#[derive(Clone)]
pub struct StaticProtect {
    pub enabled: bool,
    pub rules: Vec<StaticRule>,
    pub schedule_task_id: u64,
    pub executor: GuardExecutor,
    pub command_registry: Arc<CommandRegistry>,
    known: HashSet<(String, String)>,
    fired: HashMap<String, Instant>,
    dry_planned: HashMap<String, (f64, f64)>,
    sweep_pending: bool,
}

impl StaticProtect {
    pub fn new(config: &StaticProtectConfig, executor: GuardExecutor) -> Self {
        Self {
            enabled: config.enabled,
            rules: config.rules.clone(),
            schedule_task_id: config.schedule_task_id,
            executor,
            command_registry: Arc::default(),
            known: HashSet::new(),
            fired: HashMap::new(),
            dry_planned: HashMap::new(),
            sweep_pending: true,
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
            self.known
                .insert((position.exchange.clone(), position.inst.clone()));

            if self.check_stop_loss(position, &rule, &key).await {
                continue;
            }
            self.maintain_take_profit(position, &rule, &key).await;
        }

        self.cleanup_gone_positions(&live_keys).await;
        self.startup_sweep(&live_keys).await;
        Ok(())
    }

    /// Returns true when the stop fired (or is pending), so take-profit
    /// maintenance is skipped for this round.
    async fn check_stop_loss(
        &mut self,
        position: &PositionSnapshot,
        rule: &StaticRule,
        key: &str,
    ) -> bool {
        let size_abs = position.size.abs();
        let offset = rule
            .sl_pct
            .map(|pct| offset_from_pct(position.avg_price, pct))
            .or_else(|| {
                rule.sl_risk
                    .and_then(|risk| offset_from_risk(risk, size_abs))
            });
        let Some(offset) = offset else {
            return false;
        };
        let stop = stop_price(position.avg_price, offset, position.is_long());
        let crossed = if position.is_long() {
            position.mark_price <= stop
        } else {
            position.mark_price >= stop
        };
        if !crossed {
            return false;
        }
        if !self.may_fire(key) {
            return true;
        }
        info!(
            exchange = position.exchange.as_str(),
            inst = position.inst.as_str(),
            mark = position.mark_price,
            stop,
            "stop-loss crossed; closing position"
        );
        match self.executor.close_pct(position, 100.0).await {
            Ok(()) => {
                self.fired.insert(key.to_string(), Instant::now());
            },
            Err(err) => error!(error = ?err, "stop-loss close failed; retried next round"),
        }
        true
    }

    async fn maintain_take_profit(
        &mut self,
        position: &PositionSnapshot,
        rule: &StaticRule,
        key: &str,
    ) {
        let size_abs = position.size.abs();
        let offset = rule
            .tp_pct
            .map(|pct| offset_from_pct(position.avg_price, pct))
            .or_else(|| {
                rule.tp_risk
                    .and_then(|risk| offset_from_risk(risk, size_abs))
            });
        let Some(offset) = offset else {
            return;
        };
        let target = target_price(position.avg_price, offset, position.is_long());
        if let Err(err) = self.ensure_tp(position, target, key).await {
            warn!(error = ?err, key, "take-profit maintenance failed; retried next round");
        }
    }

    async fn ensure_tp(
        &mut self,
        position: &PositionSnapshot,
        target: f64,
        key: &str,
    ) -> InfraResult<()> {
        let size_abs = position.size.abs();

        if self.executor.is_dry_run() {
            let planned = self.dry_planned.get(key);
            if planned != Some(&(target, size_abs)) {
                self.executor.place_tp_limit(position, target).await?;
                self.dry_planned.insert(key.to_string(), (target, size_abs));
            }
            return Ok(());
        }

        let (tick, lot) = self
            .executor
            .instrument_steps(&position.exchange, &position.inst)?;
        let own = self
            .executor
            .own_open_orders(&position.exchange, &position.inst)
            .await?;
        let covered = own.len() == 1
            && own.iter().all(|order| {
                let remaining = order.size - order.executed_size;
                (order.price - target).abs() <= tick * 1.01
                    && (remaining - size_abs).abs() <= lot * 1.01
            });
        if covered {
            return Ok(());
        }
        if !own.is_empty() {
            self.executor
                .cancel_own_orders(&position.exchange, &position.inst)
                .await?;
        }
        self.executor.place_tp_limit(position, target).await?;
        Ok(())
    }

    async fn cleanup_gone_positions(&mut self, live_keys: &HashSet<String>) {
        let gone: Vec<(String, String)> = self
            .known
            .iter()
            .filter(|(exchange, inst)| !live_keys.contains(&position_key(exchange, inst)))
            .cloned()
            .collect();
        for (exchange, inst) in gone {
            match self.executor.cancel_own_orders(&exchange, &inst).await {
                Ok(count) => {
                    if count > 0 {
                        info!(exchange, inst, count, "position gone; cancelled protection");
                    }
                    let key = position_key(&exchange, &inst);
                    self.known.remove(&(exchange, inst));
                    self.fired.remove(&key);
                    self.dry_planned.remove(&key);
                },
                Err(err) => {
                    warn!(
                        ?err,
                        exchange, inst, "orphan cleanup failed; retried next round"
                    );
                },
            }
        }
    }

    /// One-time sweep after start: explicit-inst rules with no position get
    /// their leftover guard orders cancelled (protection placed before a
    /// restart for positions closed while guard was down).
    async fn startup_sweep(&mut self, live_keys: &HashSet<String>) {
        if !self.sweep_pending {
            return;
        }
        self.sweep_pending = false;
        let exchanges: Vec<String> = self.executor.exchanges().map(str::to_string).collect();
        for exchange in exchanges {
            for rule in self.rules.clone() {
                if rule.inst == "*" || live_keys.contains(&position_key(&exchange, &rule.inst)) {
                    continue;
                }
                match self.executor.cancel_own_orders(&exchange, &rule.inst).await {
                    Ok(count) if count > 0 => {
                        info!(
                            exchange,
                            inst = rule.inst.as_str(),
                            count,
                            "startup orphan cleanup"
                        );
                    },
                    Ok(_) => {},
                    Err(err) => {
                        warn!(
                            ?err,
                            exchange,
                            inst = rule.inst.as_str(),
                            "startup sweep failed"
                        );
                    },
                }
            }
        }
    }

    fn may_fire(&self, key: &str) -> bool {
        !self
            .fired
            .get(key)
            .is_some_and(|at| at.elapsed() < REFIRE_AFTER)
    }
}

fn match_rule(rules: &[StaticRule], inst: &str) -> Option<StaticRule> {
    rules
        .iter()
        .find(|rule| rule.inst == inst)
        .or_else(|| rules.iter().find(|rule| rule.inst == "*"))
        .cloned()
}
