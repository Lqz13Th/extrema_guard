use std::{env::current_dir, fs};

use serde::Deserialize;

use extrema_infra::prelude::*;

#[derive(Clone, Debug, Deserialize)]
pub struct GuardConfig {
    pub guard: GuardSection,
    #[serde(default)]
    pub static_protect: StaticProtectSection,
    #[serde(default)]
    pub profit_lock: ProfitLockSection,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GuardSection {
    pub mode: RunMode,
    #[serde(default)]
    pub i_understand_live_orders: bool,
    pub exchanges: Vec<String>,
    pub poll_seconds: u64,
    pub schedule_task_id: u64,
    #[serde(default = "default_max_order_notional")]
    pub max_order_notional: f64,
    #[serde(default = "default_min_action_interval_ms")]
    pub min_action_interval_ms: u64,
    #[serde(default = "default_action_log")]
    pub action_log: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RunMode {
    DryRun,
    Live,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct StaticProtectSection {
    #[serde(default)]
    pub rules: Vec<StaticRule>,
}

/// One protection rule per instrument, applied on every configured exchange.
/// Exactly one of `sl_pct` / `sl_risk` is required; `tp_pct` / `tp_risk` are
/// optional and also mutually exclusive. `*_risk` values are quote-currency
/// amounts for the whole position: offset = risk / |size|.
#[derive(Clone, Debug, Deserialize)]
pub struct StaticRule {
    pub inst: String,
    pub sl_pct: Option<f64>,
    pub sl_risk: Option<f64>,
    pub tp_pct: Option<f64>,
    pub tp_risk: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ProfitLockSection {
    #[serde(default)]
    pub rules: Vec<ProfitLockRule>,
}

/// Once price has moved `activation_pct` past entry in the profitable
/// direction, keep a protective exit armed at `lock_pct` past entry. The
/// armed flag lives on an in-memory high-water mark for the position
/// lifecycle; a restart re-accumulates it from the current mark.
#[derive(Clone, Debug, Deserialize)]
pub struct ProfitLockRule {
    pub inst: String,
    pub activation_pct: f64,
    pub lock_pct: f64,
}

impl StaticRule {
    pub fn validate(&self) -> InfraResult<()> {
        match (self.sl_pct, self.sl_risk) {
            (Some(_), Some(_)) => {
                return Err(InfraError::Msg(format!(
                    "rule {}: sl_pct and sl_risk are mutually exclusive",
                    self.inst
                )));
            },
            (None, None) => {
                return Err(InfraError::Msg(format!(
                    "rule {}: one of sl_pct or sl_risk is required",
                    self.inst
                )));
            },
            _ => {},
        }
        if self.tp_pct.is_some() && self.tp_risk.is_some() {
            return Err(InfraError::Msg(format!(
                "rule {}: tp_pct and tp_risk are mutually exclusive",
                self.inst
            )));
        }
        Ok(())
    }
}

impl GuardConfig {
    pub fn validate(&self) -> InfraResult<()> {
        if self.guard.exchanges.is_empty() {
            return Err(InfraError::Msg(
                "guard.exchanges must list at least one exchange".to_string(),
            ));
        }
        if self.guard.mode == RunMode::Live && !self.guard.i_understand_live_orders {
            return Err(InfraError::Msg(
                "live mode requires guard.i_understand_live_orders = true".to_string(),
            ));
        }
        for rule in &self.static_protect.rules {
            rule.validate()?;
        }
        for rule in &self.profit_lock.rules {
            if rule.lock_pct >= rule.activation_pct {
                return Err(InfraError::Msg(format!(
                    "rule {}: lock_pct must be below activation_pct",
                    rule.inst
                )));
            }
        }
        Ok(())
    }
}

/// Loads `guard.toml` from the working directory. Missing or malformed
/// configuration is a hard error, never a silent default.
pub fn load_guard_config() -> InfraResult<GuardConfig> {
    let path = current_dir().map_err(InfraError::Io)?.join("guard.toml");
    let raw = fs::read_to_string(&path)
        .map_err(|err| InfraError::Msg(format!("read {}: {err}", path.display())))?;
    let config: GuardConfig = toml::from_str(&raw)
        .map_err(|err| InfraError::Msg(format!("parse {}: {err}", path.display())))?;
    config.validate()?;
    Ok(config)
}

fn default_max_order_notional() -> f64 {
    250_000.0
}

fn default_min_action_interval_ms() -> u64 {
    1_000
}

fn default_action_log() -> String {
    "guard_actions.log".to_string()
}
