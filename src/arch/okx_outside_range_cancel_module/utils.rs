use std::{
    collections::{HashMap, HashSet},
    env::current_dir,
    fs,
    io::Write,
    path::Path,
};

use serde::{Deserialize, Serialize};

use extrema_infra::prelude::*;

#[derive(Clone, Debug, Deserialize)]
pub struct OkxOutsideRangeCancelConfig {
    #[serde(default)]
    pub enabled: bool,
    pub schedule_duration_sec: u64,
    pub schedule_task_id: u64,
    #[serde(default = "default_outside_seconds")]
    pub outside_seconds: u64,
    #[serde(default = "default_state_path")]
    pub state_path: String,
}

#[derive(Deserialize)]
struct StrategyConfigToml {
    okx_outside_range_cancel: OkxOutsideRangeCancelConfig,
}

impl OkxOutsideRangeCancelConfig {
    pub fn validate(&self) -> InfraResult<()> {
        if self.schedule_duration_sec == 0 || self.outside_seconds == 0 {
            return Err(InfraError::Msg(
                "OKX outside-range schedule and timeout must be positive".to_string(),
            ));
        }
        if self.state_path.trim().is_empty() {
            return Err(InfraError::Msg(
                "okx_outside_range_cancel.state_path must not be empty".to_string(),
            ));
        }
        Ok(())
    }
}

pub fn load_okx_outside_range_cancel_config() -> InfraResult<OkxOutsideRangeCancelConfig> {
    let path = current_dir()
        .map_err(InfraError::Io)?
        .join("strategy_config.toml");
    let raw = fs::read_to_string(&path)
        .map_err(|err| InfraError::Msg(format!("read {}: {err}", path.display())))?;
    let wrapper: StrategyConfigToml = toml::from_str(&raw)
        .map_err(|err| InfraError::Msg(format!("parse {}: {err}", path.display())))?;
    wrapper.okx_outside_range_cancel.validate()?;
    Ok(wrapper.okx_outside_range_cancel)
}

#[derive(Clone, Debug)]
pub(crate) struct BracketInput {
    pub fail_code: Option<String>,
    pub unsupported: bool,
    pub take_profit: Option<f64>,
    pub stop_loss: Option<f64>,
}

#[derive(Clone, Debug)]
pub(crate) struct OrderInput {
    pub inst_type: InstrumentType,
    pub venue_inst: String,
    pub inst: String,
    pub order_id: String,
    pub client_order_id: Option<String>,
    pub side: String,
    pub position_side: Option<String>,
    pub order_type: String,
    pub state: String,
    pub category: Option<String>,
    pub source: Option<String>,
    pub is_tp_limit: Option<bool>,
    pub price: Option<f64>,
    pub executed_size: Option<f64>,
    pub reduce_only: Option<bool>,
    pub created_time_us: Option<u64>,
    pub updated_time_us: Option<u64>,
    pub attached: Vec<BracketInput>,
    pub top_level_bracket: BracketInput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Direction {
    Long,
    Short,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OkxPositionMode {
    Net,
    LongShort,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Breach {
    TakeProfit,
    StopLoss,
}

#[derive(Clone, Debug)]
pub(crate) struct GuardOrder {
    pub inst_type: InstrumentType,
    pub venue_inst: String,
    pub inst: String,
    pub order_id: String,
    pub direction: Direction,
    pub entry: f64,
    pub take_profit: f64,
    pub stop_loss: f64,
    pub fingerprint: String,
}

pub(crate) fn guard_order(
    raw: &OrderInput,
    position_mode: OkxPositionMode,
    net_position: Option<f64>,
) -> Result<GuardOrder, &'static str> {
    if !matches!(
        raw.inst_type,
        InstrumentType::Perpetual | InstrumentType::Futures
    ) {
        return Err("unsupported_instrument_type");
    }
    if raw.state != "live" || !matches!(raw.order_type.as_str(), "limit" | "post_only") {
        return Err("not_a_live_limit_order");
    }
    if raw.reduce_only != Some(false) || raw.is_tp_limit != Some(false) {
        return Err("not_an_entry_parent");
    }
    if raw.executed_size != Some(0.0) {
        return Err("order_has_fills_or_unknown_fill_size");
    }
    if raw
        .category
        .as_deref()
        .is_some_and(|value| !value.is_empty() && value != "normal")
        || raw.source.as_deref().is_some_and(|value| !value.is_empty())
    {
        return Err("nonstandard_order");
    }
    let entry = raw.price.ok_or("missing_entry_price")?;
    let bracket = match raw.attached.as_slice() {
        [] => &raw.top_level_bracket,
        [bracket] => bracket,
        _ => return Err("split_attached_orders_not_supported"),
    };
    if bracket
        .fail_code
        .as_deref()
        .is_some_and(|code| !code.is_empty() && code != "0")
    {
        return Err("attached_order_failed");
    }
    if bracket.unsupported {
        return Err("dynamic_bracket_not_supported");
    }
    let take_profit = bracket.take_profit.ok_or("missing_take_profit")?;
    let stop_loss = bracket.stop_loss.ok_or("missing_stop_loss")?;
    let direction = order_direction(raw, position_mode, net_position)?;
    let valid_geometry = match direction {
        Direction::Long => stop_loss < entry && entry < take_profit,
        Direction::Short => take_profit < entry && entry < stop_loss,
    };
    if !valid_geometry {
        return Err("invalid_bracket_geometry");
    }
    if raw.order_id.is_empty() {
        return Err("missing_order_id");
    }
    let fingerprint = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}",
        raw.inst,
        raw.order_id,
        raw.client_order_id.as_deref().unwrap_or(""),
        raw.side,
        entry,
        take_profit,
        stop_loss,
        raw.created_time_us.unwrap_or_default(),
        raw.updated_time_us.unwrap_or_default()
    );
    Ok(GuardOrder {
        inst_type: raw.inst_type.clone(),
        venue_inst: raw.venue_inst.clone(),
        inst: raw.inst.clone(),
        order_id: raw.order_id.clone(),
        direction,
        entry,
        take_profit,
        stop_loss,
        fingerprint,
    })
}

fn order_direction(
    raw: &OrderInput,
    position_mode: OkxPositionMode,
    net_position: Option<f64>,
) -> Result<Direction, &'static str> {
    let position_side = raw.position_side.as_deref().unwrap_or_default();
    match position_mode {
        OkxPositionMode::Net => {
            let net_position = net_position
                .filter(|position| position.is_finite())
                .ok_or("net_position_context_missing")?;
            if !matches!(position_side, "" | "net") {
                return Err("unexpected_position_side_for_net_mode");
            }
            match raw.side.as_str() {
                "sell" if net_position > 0.0 => Err("net_order_may_reduce_or_reverse_long"),
                "buy" if net_position < 0.0 => Err("net_order_may_reduce_or_reverse_short"),
                "buy" => Ok(Direction::Long),
                "sell" => Ok(Direction::Short),
                _ => Err("unsupported_side"),
            }
        },
        OkxPositionMode::LongShort => match (raw.side.as_str(), position_side) {
            ("buy", "long") => Ok(Direction::Long),
            ("sell", "short") => Ok(Direction::Short),
            ("sell", "long") | ("buy", "short") => Err("hedge_mode_close_order"),
            _ => Err("ambiguous_hedge_mode_order"),
        },
    }
}

pub(crate) fn breach(order: &GuardOrder, last: f64) -> Option<Breach> {
    match order.direction {
        Direction::Long if last > order.take_profit => Some(Breach::TakeProfit),
        Direction::Long if last < order.stop_loss => Some(Breach::StopLoss),
        Direction::Short if last < order.take_profit => Some(Breach::TakeProfit),
        Direction::Short if last > order.stop_loss => Some(Breach::StopLoss),
        _ => None,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Observation {
    fingerprint: String,
    breach: Breach,
    outside_since_us: u64,
    last_seen_us: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct OutsideTracker {
    schema_version: u32,
    orders: HashMap<String, Observation>,
}

impl Default for OutsideTracker {
    fn default() -> Self {
        Self {
            schema_version: 1,
            orders: HashMap::new(),
        }
    }
}

impl OutsideTracker {
    pub fn load(path: &Path) -> InfraResult<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path).map_err(InfraError::Io)?;
        let state: Self = toml::from_str(&raw)
            .map_err(|err| InfraError::Msg(format!("parse {}: {err}", path.display())))?;
        if state.schema_version != 1
            || state
                .orders
                .values()
                .any(|value| value.outside_since_us > value.last_seen_us)
        {
            return Err(InfraError::Msg(format!(
                "invalid outside-range state: {}",
                path.display()
            )));
        }
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> InfraResult<()> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(InfraError::Io)?;
        }
        let raw = toml::to_string(self)
            .map_err(|err| InfraError::Msg(format!("serialize guard state: {err}")))?;
        let temporary = path.with_extension("tmp");
        let mut file = fs::File::create(&temporary).map_err(InfraError::Io)?;
        file.write_all(raw.as_bytes()).map_err(InfraError::Io)?;
        file.sync_all().map_err(InfraError::Io)?;
        fs::rename(temporary, path).map_err(InfraError::Io)
    }

    pub fn retain(&mut self, order_ids: &HashSet<String>) {
        self.orders
            .retain(|order_id, _| order_ids.contains(order_id));
    }

    pub fn reset(&mut self, order_id: &str) -> bool {
        self.orders.remove(order_id).is_some()
    }

    pub fn observe(
        &mut self,
        order: &GuardOrder,
        breach: Breach,
        now_us: u64,
        max_gap_us: u64,
    ) -> (u64, Option<&'static str>) {
        let existing = self.orders.get(&order.order_id);
        let reset_reason = match existing {
            None => Some("first_outside_observation"),
            Some(value) if value.fingerprint != order.fingerprint => Some("order_changed"),
            Some(value) if value.breach != breach => Some("breach_side_changed"),
            Some(value) if now_us < value.last_seen_us => Some("clock_moved_backwards"),
            Some(value) if now_us - value.last_seen_us > max_gap_us => {
                Some("observation_gap_too_large")
            },
            Some(_) => None,
        };
        let outside_since_us = if reset_reason.is_some() {
            now_us
        } else {
            existing.map_or(now_us, |value| value.outside_since_us)
        };
        self.orders.insert(
            order.order_id.clone(),
            Observation {
                fingerprint: order.fingerprint.clone(),
                breach,
                outside_since_us,
                last_seen_us: now_us,
            },
        );
        (now_us.saturating_sub(outside_since_us), reset_reason)
    }

    pub fn is_ready(&self, order: &GuardOrder, breach: Breach, now_us: u64, wait_us: u64) -> bool {
        self.orders.get(&order.order_id).is_some_and(|value| {
            value.fingerprint == order.fingerprint
                && value.breach == breach
                && now_us.saturating_sub(value.outside_since_us) >= wait_us
        })
    }
}

fn default_outside_seconds() -> u64 {
    37 * 60
}

fn default_state_path() -> String {
    "outside_range_state.toml".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(side: &str, entry: f64, take_profit: f64, stop_loss: f64) -> OrderInput {
        OrderInput {
            inst_type: InstrumentType::Perpetual,
            venue_inst: "ETH-USDT-SWAP".to_string(),
            inst: "ETH_USDT_PERP".to_string(),
            order_id: "42".to_string(),
            client_order_id: Some("manual42".to_string()),
            side: side.to_string(),
            position_side: Some("net".to_string()),
            order_type: "limit".to_string(),
            state: "live".to_string(),
            category: Some("normal".to_string()),
            source: Some(String::new()),
            is_tp_limit: Some(false),
            price: Some(entry),
            executed_size: Some(0.0),
            reduce_only: Some(false),
            created_time_us: Some(1),
            updated_time_us: Some(1),
            attached: vec![BracketInput {
                fail_code: None,
                unsupported: false,
                take_profit: Some(take_profit),
                stop_loss: Some(stop_loss),
            }],
            top_level_bracket: BracketInput {
                fail_code: None,
                unsupported: false,
                take_profit: None,
                stop_loss: None,
            },
        }
    }

    #[test]
    fn boundaries_are_strict_for_long_and_short() {
        let long = guard_order(
            &input("buy", 100.0, 110.0, 90.0),
            OkxPositionMode::Net,
            Some(0.0),
        )
        .unwrap();
        assert_eq!(breach(&long, 110.0), None);
        assert_eq!(breach(&long, 110.01), Some(Breach::TakeProfit));
        assert_eq!(breach(&long, 89.99), Some(Breach::StopLoss));

        let short = guard_order(
            &input("sell", 100.0, 90.0, 110.0),
            OkxPositionMode::Net,
            Some(0.0),
        )
        .unwrap();
        assert_eq!(breach(&short, 90.0), None);
        assert_eq!(breach(&short, 89.99), Some(Breach::TakeProfit));
        assert_eq!(breach(&short, 110.01), Some(Breach::StopLoss));
    }

    #[test]
    fn partial_fill_and_invalid_geometry_are_rejected() {
        let mut partial = input("buy", 100.0, 110.0, 90.0);
        partial.executed_size = Some(0.1);
        assert!(guard_order(&partial, OkxPositionMode::Net, Some(0.0)).is_err());
        assert!(
            guard_order(
                &input("buy", 100.0, 90.0, 110.0),
                OkxPositionMode::Net,
                Some(0.0),
            )
            .is_err()
        );
    }

    #[test]
    fn dynamic_brackets_are_rejected() {
        let mut dynamic = input("buy", 100.0, 110.0, 90.0);
        dynamic.attached[0].unsupported = true;
        assert_eq!(
            guard_order(&dynamic, OkxPositionMode::Net, Some(0.0)).unwrap_err(),
            "dynamic_bracket_not_supported"
        );
    }

    #[test]
    fn net_mode_rejects_orders_that_may_reduce_or_reverse() {
        let buy = input("buy", 100.0, 110.0, 90.0);
        let sell = input("sell", 100.0, 90.0, 110.0);

        assert!(guard_order(&buy, OkxPositionMode::Net, Some(2.0)).is_ok());
        assert_eq!(
            guard_order(&sell, OkxPositionMode::Net, Some(2.0)).unwrap_err(),
            "net_order_may_reduce_or_reverse_long"
        );
        assert!(guard_order(&sell, OkxPositionMode::Net, Some(-2.0)).is_ok());
        assert_eq!(
            guard_order(&buy, OkxPositionMode::Net, Some(-2.0)).unwrap_err(),
            "net_order_may_reduce_or_reverse_short"
        );

        let mut unexpected_side = buy.clone();
        unexpected_side.position_side = Some("long".to_string());
        assert_eq!(
            guard_order(&unexpected_side, OkxPositionMode::Net, Some(0.0)).unwrap_err(),
            "unexpected_position_side_for_net_mode"
        );
        assert_eq!(
            guard_order(&buy, OkxPositionMode::Net, None).unwrap_err(),
            "net_position_context_missing"
        );
    }

    #[test]
    fn hedge_mode_accepts_only_open_direction_pairs() {
        let mut long_open = input("buy", 100.0, 110.0, 90.0);
        long_open.position_side = Some("long".to_string());
        assert!(guard_order(&long_open, OkxPositionMode::LongShort, None).is_ok());

        let mut short_open = input("sell", 100.0, 90.0, 110.0);
        short_open.position_side = Some("short".to_string());
        assert!(guard_order(&short_open, OkxPositionMode::LongShort, None).is_ok());

        long_open.side = "sell".to_string();
        assert_eq!(
            guard_order(&long_open, OkxPositionMode::LongShort, None).unwrap_err(),
            "hedge_mode_close_order"
        );

        short_open.side = "buy".to_string();
        assert_eq!(
            guard_order(&short_open, OkxPositionMode::LongShort, None).unwrap_err(),
            "hedge_mode_close_order"
        );
    }

    #[test]
    fn timer_requires_continuous_same_side_observations() {
        let order = guard_order(
            &input("buy", 100.0, 110.0, 90.0),
            OkxPositionMode::Net,
            Some(0.0),
        )
        .unwrap();
        let mut tracker = OutsideTracker::default();
        let (first, _) = tracker.observe(&order, Breach::TakeProfit, 1_000_000, 90_000_000);
        let (elapsed, _) =
            tracker.observe(&order, Breach::TakeProfit, 2_221_000_000, 3_000_000_000);
        assert_eq!(first, 0);
        assert_eq!(elapsed, 2_220_000_000);

        let (reset, reason) = tracker.observe(&order, Breach::StopLoss, 2_222_000_000, 90_000_000);
        assert_eq!(reset, 0);
        assert_eq!(reason, Some("breach_side_changed"));
    }
}
