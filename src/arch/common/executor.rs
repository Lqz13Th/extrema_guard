use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use tracing::{info, warn};

use extrema_infra::{
    arch::market_assets::{
        api_data::{
            account_data::{OrderAckData, OrderDetailData},
            utils_data::InstrumentInfo,
        },
        api_general::{OrderParams, normalize_to_string, normalize_to_string_reduce_only},
        exchange::prelude::*,
    },
    prelude::*,
};

use super::{
    config::{GuardConfig, RunMode},
    report::ActionLog,
    safety::SafetyLimits,
};

/// "guard" in hex; Hyperliquid cloids must be 0x-prefixed 128-bit hex.
const OWN_ID_HEX_PREFIX: &str = "0x6775617264";

static ORDER_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct PositionSnapshot {
    pub exchange: String,
    pub inst: String,
    /// Signed: positive long, negative short.
    pub size: f64,
    pub avg_price: f64,
    pub mark_price: f64,
}

impl PositionSnapshot {
    pub fn is_long(&self) -> bool {
        self.size > 0.0
    }

    pub fn notional(&self) -> f64 {
        self.size.abs() * self.mark_price
    }
}

struct ExchangeSlot {
    name: String,
    client: LobClients,
    instruments: HashMap<String, InstrumentInfo>,
}

/// Shared exchange access for guard modules.
///
/// The executor centralizes credentials, dry-run behavior, action throttling,
/// instrument metadata, and audit logging. Individual modules define which
/// account actions are eligible for their policy.
#[derive(Clone)]
pub struct GuardExecutor {
    slots: Arc<Vec<ExchangeSlot>>,
    limits: SafetyLimits,
    dry_run: bool,
    log: ActionLog,
    last_action: Arc<Mutex<HashMap<String, Instant>>>,
}

impl GuardExecutor {
    /// Builds one client per configured exchange, loads API keys from the
    /// environment, and warms the per-venue instrument metadata cache used
    /// for tick/lot rounding. Fails hard if metadata cannot be fetched.
    pub async fn connect(config: &GuardConfig) -> InfraResult<Self> {
        let mut slots = Vec::with_capacity(config.guard.exchanges.len());
        for name in &config.guard.exchanges {
            let mut client = lob_client_for(name)?;
            client.init_api_key();
            let infos = client
                .get_instrument_info(InstrumentType::Perpetual)
                .await
                .map_err(|err| InfraError::Msg(format!("{name}: fetch instrument info: {err}")))?;
            let instruments: HashMap<String, InstrumentInfo> = infos
                .into_iter()
                .map(|info| (info.inst.clone(), info))
                .collect();
            info!(
                exchange = name.as_str(),
                instruments = instruments.len(),
                "exchange connected"
            );
            slots.push(ExchangeSlot {
                name: name.clone(),
                client,
                instruments,
            });
        }
        Ok(Self {
            slots: Arc::new(slots),
            limits: SafetyLimits::from_config(config),
            dry_run: config.guard.mode == RunMode::DryRun,
            log: ActionLog::new(&config.guard.action_log),
            last_action: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    pub fn limits(&self) -> &SafetyLimits {
        &self.limits
    }

    pub fn exchanges(&self) -> impl Iterator<Item = &str> {
        self.slots.iter().map(|slot| slot.name.as_str())
    }

    /// All open perp positions across configured exchanges, sizes signed.
    /// Entries with insane prices or unknown side are skipped with a warning
    /// rather than propagated: a malformed row must never look like a
    /// position worth protecting.
    pub async fn positions(&self) -> InfraResult<Vec<PositionSnapshot>> {
        let mut out = Vec::new();
        for slot in self.slots.iter() {
            let positions =
                slot.client.get_positions(None).await.map_err(|err| {
                    InfraError::Msg(format!("{}: get positions: {err}", slot.name))
                })?;
            for position in positions {
                if position.size == 0.0 {
                    continue;
                }
                let signed_size = match position.position_side {
                    PositionSide::Long => position.size.abs(),
                    PositionSide::Short => -position.size.abs(),
                    PositionSide::Both => position.size,
                    PositionSide::Unknown => {
                        warn!(
                            exchange = slot.name.as_str(),
                            inst = position.inst.as_str(),
                            "skipping position with unknown side"
                        );
                        continue;
                    },
                };
                if !SafetyLimits::price_is_sane(position.avg_price)
                    || !SafetyLimits::price_is_sane(position.mark_price)
                {
                    warn!(
                        exchange = slot.name.as_str(),
                        inst = position.inst.as_str(),
                        avg_price = position.avg_price,
                        mark_price = position.mark_price,
                        "skipping position with insane prices"
                    );
                    continue;
                }
                out.push(PositionSnapshot {
                    exchange: slot.name.clone(),
                    inst: position.inst,
                    size: signed_size,
                    avg_price: position.avg_price,
                    mark_price: position.mark_price,
                });
            }
        }
        Ok(out)
    }

    /// All open orders for one instrument.
    pub async fn open_orders(
        &self,
        exchange: &str,
        inst: &str,
    ) -> InfraResult<Vec<OrderDetailData>> {
        let slot = self.slot(exchange)?;
        slot.client
            .get_open_orders(inst, None)
            .await
            .map_err(|err| InfraError::Msg(format!("{exchange}: get open orders {inst}: {err}")))
    }

    /// Guard-owned open orders for one instrument, filtered by the per-venue
    /// client-order-id prefix. Built-in protection modules use this helper to
    /// reconcile only the orders they created.
    pub async fn own_open_orders(
        &self,
        exchange: &str,
        inst: &str,
    ) -> InfraResult<Vec<OrderDetailData>> {
        Ok(self
            .open_orders(exchange, inst)
            .await?
            .into_iter()
            .filter(|order| {
                order
                    .cli_order_id
                    .as_deref()
                    .is_some_and(|id| is_own_id(exchange, id))
            })
            .collect())
    }

    /// Rests a reduce-only take-profit limit for the full position. Returns
    /// the guard client order id, or `None` when nothing was placed (dry-run,
    /// or position below the venue minimum).
    pub async fn place_tp_limit(
        &self,
        position: &PositionSnapshot,
        price: f64,
    ) -> InfraResult<Option<String>> {
        if !SafetyLimits::price_is_sane(price) {
            return Err(InfraError::Msg(format!(
                "{} {}: refusing tp at insane price {price}",
                position.exchange, position.inst
            )));
        }
        let slot = self.slot(&position.exchange)?;
        let info = self.instrument(slot, &position.inst)?;
        let size = normalize_to_string_reduce_only(position.size.abs(), info.lot_size);
        let price_str = normalize_to_string(price, info.tick_size);
        let size_num: f64 = size.parse().unwrap_or(0.0);
        if size_num <= 0.0 || size_num < info.min_lmt_size {
            warn!(
                exchange = position.exchange.as_str(),
                inst = position.inst.as_str(),
                size = size.as_str(),
                min = info.min_lmt_size,
                "position below venue minimum; tp not placed"
            );
            return Ok(None);
        }
        self.check_notional(position, size_num * price)?;

        let side = if position.is_long() {
            OrderSide::SELL
        } else {
            OrderSide::BUY
        };
        let cli_order_id = own_id_for(&position.exchange);
        let action = format!(
            "tp_limit {} {} {:?} {} @ {} cli_id={}",
            position.exchange, position.inst, side, size, price_str, cli_order_id
        );
        if self.dry_run {
            self.log.record(&format!("DRY-RUN {action}"));
            return Ok(None);
        }
        self.throttle(&position.exchange).await;
        let ack = slot
            .client
            .place_order(OrderParams {
                inst: position.inst.clone(),
                side,
                size,
                order_type: OrderType::Limit,
                price: Some(price_str),
                reduce_only: Some(true),
                time_in_force: Some(TimeInForce::GTC),
                client_order_id: Some(cli_order_id.clone()),
                ..OrderParams::default()
            })
            .await
            .map_err(|err| InfraError::Msg(format!("{action}: {err}")))?;
        self.log.record(&format!(
            "{action} -> ack order_id={} status={:?}",
            ack.order_id, ack.order_status
        ));
        Ok(Some(cli_order_id))
    }

    /// Closes a percentage of a position with a reduce-only market order.
    pub async fn close_pct(&self, position: &PositionSnapshot, pct: f64) -> InfraResult<()> {
        if !(0.0..=100.0).contains(&pct) || pct == 0.0 {
            return Err(InfraError::Msg(format!("invalid close pct: {pct}")));
        }
        let slot = self.slot(&position.exchange)?;
        let info = self.instrument(slot, &position.inst)?;
        let raw_size = position.size.abs() * pct / 100.0;
        let size = normalize_to_string_reduce_only(raw_size, info.lot_size);
        let size_num: f64 = size.parse().unwrap_or(0.0);
        if size_num <= 0.0 || size_num < info.min_mkt_size {
            warn!(
                exchange = position.exchange.as_str(),
                inst = position.inst.as_str(),
                size = size.as_str(),
                min = info.min_mkt_size,
                "close size below venue minimum; not sent"
            );
            return Ok(());
        }
        self.check_notional(position, size_num * position.mark_price)?;

        let side = if position.is_long() {
            OrderSide::SELL
        } else {
            OrderSide::BUY
        };
        let cli_order_id = own_id_for(&position.exchange);
        let action = format!(
            "close {}% {} {} {:?} {} cli_id={}",
            pct, position.exchange, position.inst, side, size, cli_order_id
        );
        if self.dry_run {
            self.log.record(&format!("DRY-RUN {action}"));
            return Ok(());
        }
        self.throttle(&position.exchange).await;
        let ack = slot
            .client
            .place_order(OrderParams {
                inst: position.inst.clone(),
                side,
                size,
                order_type: OrderType::Market,
                reduce_only: Some(true),
                client_order_id: Some(cli_order_id.clone()),
                ..OrderParams::default()
            })
            .await
            .map_err(|err| InfraError::Msg(format!("{action}: {err}")))?;
        self.log.record(&format!(
            "{action} -> ack order_id={} status={:?}",
            ack.order_id, ack.order_status
        ));
        Ok(())
    }

    /// Cancels one exact order selected by a module policy. At least one of
    /// `order_id` or `cli_order_id` must be present. Dry-run records the action
    /// and returns `None` without sending a request.
    pub async fn cancel_order(
        &self,
        exchange: &str,
        inst: &str,
        order_id: Option<&str>,
        cli_order_id: Option<&str>,
    ) -> InfraResult<Option<OrderAckData>> {
        let order_id = order_id.filter(|id| !id.trim().is_empty());
        let cli_order_id = cli_order_id.filter(|id| !id.trim().is_empty());
        if order_id.is_none() && cli_order_id.is_none() {
            return Err(InfraError::Msg(
                "cancel requires order_id or cli_order_id".to_string(),
            ));
        }

        let slot = self.slot(exchange)?;
        let action = format!(
            "cancel {exchange} {inst} order_id={} cli_id={}",
            order_id.unwrap_or(""),
            cli_order_id.unwrap_or("")
        );
        if self.dry_run {
            self.log.record(&format!("DRY-RUN {action}"));
            return Ok(None);
        }

        self.throttle(exchange).await;
        let ack = slot
            .client
            .cancel_order(inst, order_id, cli_order_id)
            .await
            .map_err(|err| InfraError::Msg(format!("{action}: {err}")))?;
        self.log.record(&format!(
            "{action} -> ack order_id={} status={:?}",
            ack.order_id, ack.order_status
        ));
        Ok(Some(ack))
    }

    /// Cancels one guard-owned order. This remains a convenience helper for
    /// modules that use guard-prefixed client order ids as their ownership
    /// policy; it is not a framework-wide cancellation restriction.
    pub async fn cancel_own_order(
        &self,
        exchange: &str,
        inst: &str,
        cli_order_id: &str,
    ) -> InfraResult<()> {
        if !is_own_id(exchange, cli_order_id) {
            return Err(InfraError::Msg(format!(
                "order {cli_order_id} is not owned by the built-in guard modules"
            )));
        }
        self.cancel_order(exchange, inst, None, Some(cli_order_id))
            .await?;
        Ok(())
    }

    /// Cancels every guard-owned open order on one instrument; returns how
    /// many cancels were attempted. Individual failures are logged and do not
    /// stop the sweep — the next reconcile round retries what remains.
    pub async fn cancel_own_orders(&self, exchange: &str, inst: &str) -> InfraResult<usize> {
        let orders = self.own_open_orders(exchange, inst).await?;
        let mut attempted = 0;
        for order in &orders {
            let Some(cli_order_id) = order.cli_order_id.as_deref() else {
                continue;
            };
            attempted += 1;
            if let Err(err) = self.cancel_own_order(exchange, inst, cli_order_id).await {
                warn!(
                    ?err,
                    exchange, inst, cli_order_id, "cancel failed; retried next round"
                );
            }
        }
        Ok(attempted)
    }

    /// `(tick_size, lot_size)` for one instrument, from the connect-time cache.
    pub fn instrument_steps(&self, exchange: &str, inst: &str) -> InfraResult<(f64, f64)> {
        let slot = self.slot(exchange)?;
        let info = self.instrument(slot, inst)?;
        Ok((info.tick_size, info.lot_size))
    }

    fn slot(&self, exchange: &str) -> InfraResult<&ExchangeSlot> {
        self.slots
            .iter()
            .find(|slot| slot.name == exchange)
            .ok_or_else(|| InfraError::Msg(format!("unknown exchange: {exchange}")))
    }

    fn instrument<'a>(
        &self,
        slot: &'a ExchangeSlot,
        inst: &str,
    ) -> InfraResult<&'a InstrumentInfo> {
        slot.instruments
            .get(inst)
            .ok_or_else(|| InfraError::Msg(format!("{}: unknown instrument {inst}", slot.name)))
    }

    fn check_notional(&self, position: &PositionSnapshot, notional: f64) -> InfraResult<()> {
        if notional > self.limits.max_order_notional {
            return Err(InfraError::Msg(format!(
                "{} {}: order notional {notional:.2} exceeds max_order_notional {}; raise the limit in guard.toml if intended",
                position.exchange, position.inst, self.limits.max_order_notional
            )));
        }
        Ok(())
    }

    async fn throttle(&self, exchange: &str) {
        let interval = Duration::from_millis(self.limits.min_action_interval_ms);
        let wait = {
            let mut map = self.last_action.lock().expect("last_action lock poisoned");
            let now = Instant::now();
            let slot_time = match map.get(exchange) {
                Some(last) if *last + interval > now => *last + interval,
                _ => now,
            };
            map.insert(exchange.to_string(), slot_time);
            slot_time.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

fn lob_client_for(name: &str) -> InfraResult<LobClients> {
    match name {
        "hyperliquid" => Ok(LobClients::Hyperliquid(HyperliquidCli::default())),
        "binance_um" => Ok(LobClients::BinanceUm(BinanceUmCli::default())),
        "okx" => Ok(LobClients::Okx(OkxCli::default())),
        "gate_futures" => Ok(LobClients::GateFutures(GateFuturesCli::default())),
        other => Err(InfraError::Msg(format!(
            "unsupported exchange: {other} (expected hyperliquid | binance_um | okx | gate_futures)"
        ))),
    }
}

/// Venue-legal guard order ids: Hyperliquid requires 0x-prefixed 128-bit hex,
/// OKX allows alphanumerics only, Gate requires a `t-` prefix.
fn own_id_for(exchange: &str) -> String {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_micros() as u64)
        .unwrap_or_default();
    let seq = ORDER_COUNTER.fetch_add(1, Ordering::Relaxed);
    match exchange {
        "hyperliquid" => format!("{OWN_ID_HEX_PREFIX}{micros:016x}{:06x}", seq & 0xFF_FFFF),
        "okx" => format!("guard{micros:x}{seq:x}"),
        "gate_futures" => format!("t-guard{micros:x}{:02x}", seq & 0xFF),
        _ => format!("guard-{micros:x}-{seq:x}"),
    }
}

pub fn position_key(exchange: &str, inst: &str) -> String {
    format!("{exchange}:{inst}")
}

pub fn is_own_id(exchange: &str, cli_order_id: &str) -> bool {
    match exchange {
        "hyperliquid" => cli_order_id
            .to_ascii_lowercase()
            .starts_with(OWN_ID_HEX_PREFIX),
        "okx" => cli_order_id.starts_with("guard"),
        "gate_futures" => cli_order_id.starts_with("t-guard"),
        _ => cli_order_id.starts_with("guard-"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_ids_are_venue_legal_and_recognized() {
        let hl = own_id_for("hyperliquid");
        assert!(hl.starts_with("0x"));
        assert_eq!(hl.len(), 2 + 32);
        assert!(hl[2..].chars().all(|c| c.is_ascii_hexdigit()));
        assert!(is_own_id("hyperliquid", &hl));

        let okx = own_id_for("okx");
        assert!(okx.chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(okx.len() <= 32);
        assert!(is_own_id("okx", &okx));

        let gate = own_id_for("gate_futures");
        assert!(gate.starts_with("t-"));
        assert!(gate.len() <= 28);
        assert!(is_own_id("gate_futures", &gate));

        let binance = own_id_for("binance_um");
        assert!(binance.len() <= 36);
        assert!(is_own_id("binance_um", &binance));
    }

    #[test]
    fn foreign_ids_are_never_own() {
        assert!(!is_own_id(
            "hyperliquid",
            "0xdeadbeef00000000000000000000dead"
        ));
        assert!(!is_own_id("okx", "myalgo123"));
        assert!(!is_own_id("gate_futures", "t-web-abc"));
        assert!(!is_own_id("binance_um", "x-gateway-42"));
    }

    #[test]
    fn snapshot_sign_and_notional() {
        let position = PositionSnapshot {
            exchange: "hyperliquid".to_string(),
            inst: "ETH_USDC_PERP".to_string(),
            size: -2.0,
            avg_price: 1_900.0,
            mark_price: 2_000.0,
        };
        assert!(!position.is_long());
        assert_eq!(position.notional(), 4_000.0);
    }
}
