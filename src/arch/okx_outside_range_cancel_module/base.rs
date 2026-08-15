use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use tracing::{debug, warn};

use extrema_infra::{
    arch::market_assets::{api_data::price_data::TickerData, exchange::prelude::*},
    prelude::*,
};

use crate::arch::executor::GuardExecutor;

use super::utils::{
    BracketInput, Breach, GuardOrder, OkxOutsideRangeCancelConfig, OrderInput, OutsideTracker,
    breach, guard_order,
};

const PAGE_LIMIT: u32 = 100;
const MAX_PRICE_AGE_US: u64 = 120_000_000;
const FUTURE_PRICE_TOLERANCE_US: u64 = 5_000_000;

#[derive(Clone)]
pub struct OkxOutsideRangeCancel {
    pub config: OkxOutsideRangeCancelConfig,
    pub executor: GuardExecutor,
    pub command_registry: Arc<CommandRegistry>,
    tracker: OutsideTracker,
    state_path: PathBuf,
    wait_us: u64,
    max_gap_us: u64,
}

impl OkxOutsideRangeCancel {
    pub fn new(config: &OkxOutsideRangeCancelConfig, executor: GuardExecutor) -> InfraResult<Self> {
        config.validate()?;
        if config.enabled && !executor.exchanges().any(|exchange| exchange == "okx") {
            return Err(InfraError::Msg(
                "okx_outside_range_cancel requires okx in guard_executor.exchanges".to_string(),
            ));
        }
        let state_path = PathBuf::from(&config.state_path);
        let tracker = if config.enabled {
            OutsideTracker::load(&state_path)?
        } else {
            OutsideTracker::default()
        };
        Ok(Self {
            config: config.clone(),
            executor,
            command_registry: Arc::default(),
            tracker,
            state_path,
            wait_us: config.outside_seconds.saturating_mul(1_000_000),
            max_gap_us: config
                .schedule_duration_sec
                .saturating_add(config.schedule_duration_sec / 2)
                .saturating_mul(1_000_000),
        })
    }

    pub(crate) async fn reconcile(&mut self) -> InfraResult<()> {
        let orders = self.fetch_guard_orders().await?;
        let order_ids = orders
            .iter()
            .map(|order| order.order_id.clone())
            .collect::<HashSet<_>>();
        self.tracker.retain(&order_ids);

        let prices = self.fetch_last_prices(&orders).await?;
        let now_us = now_micros()?;
        let mut candidates = Vec::new();
        for order in &orders {
            let Some(last) = usable_last(prices.get(&order.inst), now_us) else {
                self.reset(order, "last_price_unavailable");
                continue;
            };
            let Some(side) = breach(order, last) else {
                self.reset(order, "price_returned_inside");
                continue;
            };
            let (elapsed_us, reset_reason) =
                self.tracker.observe(order, side, now_us, self.max_gap_us);
            if let Some(reason) = reset_reason {
                self.executor.audit(&format!(
                    "outside_range tracking_started inst={} order_id={} side={side:?} last={last} tp={} sl={} reason={reason}",
                    order.inst, order.order_id, order.take_profit, order.stop_loss
                ));
            }
            if elapsed_us >= self.wait_us {
                candidates.push((order.clone(), side));
            }
        }
        self.tracker.save(&self.state_path)?;

        if !candidates.is_empty() {
            self.cancel_after_preflight(candidates).await?;
        }
        Ok(())
    }

    async fn cancel_after_preflight(
        &mut self,
        candidates: Vec<(GuardOrder, Breach)>,
    ) -> InfraResult<()> {
        let current = self
            .fetch_guard_orders()
            .await?
            .into_iter()
            .map(|order| (order.order_id.clone(), order))
            .collect::<HashMap<_, _>>();
        let preflight_orders = candidates
            .iter()
            .filter_map(|(candidate, _)| current.get(&candidate.order_id).cloned())
            .collect::<Vec<_>>();
        let prices = self.fetch_last_prices(&preflight_orders).await?;
        let now_us = now_micros()?;
        let mut state_changed = false;

        for (candidate, expected_side) in candidates {
            let Some(order) = current.get(&candidate.order_id) else {
                state_changed |= self.tracker.reset(&candidate.order_id);
                continue;
            };
            let Some(last) = usable_last(prices.get(&order.inst), now_us) else {
                self.reset(order, "preflight_last_price_unavailable");
                state_changed = true;
                continue;
            };
            if order.fingerprint != candidate.fingerprint
                || breach(order, last) != Some(expected_side)
                || !self
                    .tracker
                    .is_ready(order, expected_side, now_us, self.wait_us)
            {
                self.reset(order, "preflight_condition_changed");
                state_changed = true;
                continue;
            }

            self.executor.audit(&format!(
                "outside_range cancel_candidate inst={} order_id={} side={expected_side:?} last={last} entry={} tp={} sl={}",
                order.inst, order.order_id, order.entry, order.take_profit, order.stop_loss
            ));
            match self
                .executor
                .cancel_order("okx", &order.inst, Some(&order.order_id), None)
                .await
            {
                Ok(_) if !self.executor.is_dry_run() => {
                    match self.cancel_is_terminal(order).await {
                        Ok(true) => {
                            state_changed |= self.tracker.reset(&order.order_id);
                        },
                        Ok(false) => warn!(
                            inst = order.inst,
                            order_id = order.order_id,
                            "cancel acknowledged but order is not terminal; reconciled next schedule"
                        ),
                        Err(err) => warn!(
                            error = ?err,
                            inst = order.inst,
                            order_id = order.order_id,
                            "cancel acknowledged but terminal confirmation failed; reconciled next schedule"
                        ),
                    }
                },
                Ok(_) => {},
                Err(err) => warn!(
                    error = ?err,
                    inst = order.inst,
                    order_id = order.order_id,
                    "outside-range cancel failed; retried next schedule"
                ),
            }
        }

        if state_changed {
            self.tracker.save(&self.state_path)?;
        }
        Ok(())
    }

    async fn fetch_guard_orders(&self) -> InfraResult<Vec<GuardOrder>> {
        let LobClients::Okx(client) = self.executor.exchange_reader("okx")? else {
            return Err(InfraError::Msg(
                "okx reader has unexpected type".to_string(),
            ));
        };
        let mut rows = client
            .get_open_orders_raw(OkxOpenOrdersReq {
                inst_type: Some("SWAP".to_string()),
                limit: Some(PAGE_LIMIT),
                ..Default::default()
            })
            .await?;
        if rows.len() == PAGE_LIMIT as usize {
            return Err(InfraError::Msg(
                "full OKX SWAP open-order page; scan would be incomplete".to_string(),
            ));
        }
        let futures = client
            .get_open_orders_raw(OkxOpenOrdersReq {
                inst_type: Some("FUTURES".to_string()),
                limit: Some(PAGE_LIMIT),
                ..Default::default()
            })
            .await?;
        if futures.len() == PAGE_LIMIT as usize {
            return Err(InfraError::Msg(
                "full OKX FUTURES open-order page; scan would be incomplete".to_string(),
            ));
        }
        rows.extend(futures);

        Ok(rows
            .into_iter()
            .filter_map(|raw| {
                let input = OrderInput {
                    inst_type: match raw.instType.as_deref() {
                        Some("SWAP") => InstrumentType::Perpetual,
                        Some("FUTURES") => InstrumentType::Futures,
                        _ => InstrumentType::Unknown,
                    },
                    venue_inst: raw.instId.clone(),
                    inst: okx_inst_to_cli(&raw.instId),
                    order_id: raw.ordId,
                    client_order_id: raw.clOrdId.filter(|value| !value.is_empty()),
                    side: raw.side,
                    order_type: raw.ordType,
                    state: raw.state,
                    category: raw.category,
                    source: raw.source,
                    is_tp_limit: raw.isTpLimit.as_deref().and_then(parse_bool),
                    price: parse_positive(raw.px.as_deref()),
                    executed_size: parse_non_negative(raw.accFillSz.as_deref()),
                    reduce_only: raw.reduceOnly.as_ref().and_then(|value| {
                        value
                            .as_bool()
                            .or_else(|| value.as_str().and_then(parse_bool))
                    }),
                    created_time_us: parse_millis(raw.cTime.as_deref()),
                    updated_time_us: parse_millis(raw.uTime.as_deref()),
                    attached: raw
                        .attachAlgoOrds
                        .into_iter()
                        .map(|algo| BracketInput {
                            fail_code: algo.failCode,
                            unsupported: [
                                algo.activePx,
                                algo.callbackRatio,
                                algo.callbackSpread,
                                algo.tpTriggerRatio,
                                algo.slTriggerRatio,
                            ]
                            .iter()
                            .any(|value| value.as_deref().is_some_and(|value| !value.is_empty())),
                            take_profit: parse_positive(algo.tpTriggerPx.as_deref())
                                .or_else(|| parse_positive(algo.tpOrdPx.as_deref())),
                            stop_loss: parse_positive(algo.slTriggerPx.as_deref()),
                        })
                        .collect(),
                    top_level_bracket: BracketInput {
                        fail_code: None,
                        unsupported: false,
                        take_profit: parse_positive(raw.tpTriggerPx.as_deref())
                            .or_else(|| parse_positive(raw.tpOrdPx.as_deref())),
                        stop_loss: parse_positive(raw.slTriggerPx.as_deref()),
                    },
                };
                match guard_order(&input) {
                    Ok(order) => Some(order),
                    Err(reason) => {
                        debug!(
                            inst = input.inst,
                            order_id = input.order_id,
                            reason,
                            "outside-range order skipped"
                        );
                        None
                    },
                }
            })
            .collect())
    }

    async fn fetch_last_prices(
        &self,
        orders: &[GuardOrder],
    ) -> InfraResult<HashMap<String, TickerData>> {
        let reader = self.executor.exchange_reader("okx")?;
        let swaps = instruments(orders, InstrumentType::Perpetual);
        let futures = instruments(orders, InstrumentType::Futures);
        let mut prices = if swaps.is_empty() {
            Vec::new()
        } else {
            reader
                .get_tickers(Some(&swaps), Some(InstrumentType::Perpetual))
                .await?
        };
        if !futures.is_empty() {
            prices.extend(
                reader
                    .get_tickers(Some(&futures), Some(InstrumentType::Futures))
                    .await?,
            );
        }
        Ok(prices
            .into_iter()
            .map(|price| (price.inst.clone(), price))
            .collect())
    }

    async fn cancel_is_terminal(&self, order: &GuardOrder) -> InfraResult<bool> {
        let LobClients::Okx(client) = self.executor.exchange_reader("okx")? else {
            return Err(InfraError::Msg(
                "okx reader has unexpected type".to_string(),
            ));
        };
        let row = client
            .get_order_raw(OkxOrderReq {
                inst_id: order.venue_inst.clone(),
                ord_id: Some(order.order_id.clone()),
                ..Default::default()
            })
            .await?
            .into_iter()
            .next();
        let Some(row) = row else {
            self.executor.audit(&format!(
                "outside_range cancel_confirmation_missing inst={} order_id={}",
                order.inst, order.order_id
            ));
            return Ok(false);
        };
        let filled = parse_non_negative(row.accFillSz.as_deref()).unwrap_or_default();
        let terminal = matches!(row.state.as_str(), "canceled" | "mmp_canceled" | "filled");
        self.executor.audit(&format!(
            "outside_range cancel_confirmation inst={} order_id={} state={} executed_size={filled}",
            order.inst, order.order_id, row.state
        ));
        Ok(terminal)
    }

    fn reset(&mut self, order: &GuardOrder, reason: &str) {
        if self.tracker.reset(&order.order_id) {
            self.executor.audit(&format!(
                "outside_range tracking_reset inst={} order_id={} reason={reason}",
                order.inst, order.order_id
            ));
        }
    }
}

fn instruments(orders: &[GuardOrder], inst_type: InstrumentType) -> Vec<String> {
    let mut instruments = orders
        .iter()
        .filter(|order| order.inst_type == inst_type)
        .map(|order| order.inst.clone())
        .collect::<Vec<_>>();
    instruments.sort_unstable();
    instruments.dedup();
    instruments
}

fn usable_last(price: Option<&TickerData>, now_us: u64) -> Option<f64> {
    let price = price?;
    (price.price.is_finite()
        && price.price > 0.0
        && price.timestamp <= now_us.saturating_add(FUTURE_PRICE_TOLERANCE_US)
        && now_us.saturating_sub(price.timestamp) <= MAX_PRICE_AGE_US)
        .then_some(price.price)
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn parse_positive(raw: Option<&str>) -> Option<f64> {
    raw.and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
}

fn parse_non_negative(raw: Option<&str>) -> Option<f64> {
    raw.and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn parse_millis(raw: Option<&str>) -> Option<u64> {
    raw.and_then(|value| value.parse::<u64>().ok())
        .and_then(|value| value.checked_mul(1_000))
}

fn now_micros() -> InfraResult<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_micros() as u64)
        .map_err(|err| InfraError::Msg(format!("system clock is before unix epoch: {err}")))
}
