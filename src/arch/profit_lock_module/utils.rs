use std::{env::current_dir, fs};

use serde::Deserialize;

use extrema_infra::prelude::*;

#[derive(Clone, Debug, Deserialize)]
pub struct ProfitLockConfig {
    #[serde(default)]
    pub enabled: bool,
    pub schedule_duration_sec: u64,
    pub schedule_task_id: u64,
    #[serde(default)]
    pub rules: Vec<ProfitLockRule>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProfitLockRule {
    pub inst: String,
    pub activation_pct: f64,
    pub lock_pct: f64,
}

#[derive(Deserialize)]
struct StrategyConfigToml {
    profit_lock: ProfitLockConfig,
}

impl ProfitLockConfig {
    pub fn validate(&self) -> InfraResult<()> {
        if self.schedule_duration_sec == 0 {
            return Err(InfraError::Msg(
                "profit_lock.schedule_duration_sec must be positive".to_string(),
            ));
        }
        for rule in &self.rules {
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

pub fn load_profit_lock_config() -> InfraResult<ProfitLockConfig> {
    let path = current_dir()
        .map_err(InfraError::Io)?
        .join("strategy_config.toml");
    let raw = fs::read_to_string(&path)
        .map_err(|err| InfraError::Msg(format!("read {}: {err}", path.display())))?;
    let wrapper: StrategyConfigToml = toml::from_str(&raw)
        .map_err(|err| InfraError::Msg(format!("parse {}: {err}", path.display())))?;
    wrapper.profit_lock.validate()?;
    Ok(wrapper.profit_lock)
}

pub fn armed(entry: f64, high_water: f64, activation_pct: f64, is_long: bool) -> bool {
    if entry <= 0.0 {
        return false;
    }
    let moved_pct = if is_long {
        (high_water - entry) / entry * 100.0
    } else {
        (entry - high_water) / entry * 100.0
    };
    moved_pct >= activation_pct
}

pub fn lock_price(entry: f64, lock_pct: f64, is_long: bool) -> f64 {
    if is_long {
        entry * (1.0 + lock_pct / 100.0)
    } else {
        entry * (1.0 - lock_pct / 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arms_only_after_activation_threshold() {
        assert!(!armed(100.0, 104.9, 5.0, true));
        assert!(armed(100.0, 105.0, 5.0, true));
        assert!(armed(100.0, 95.0, 5.0, false));
        assert!(!armed(100.0, 95.1, 5.0, false));
    }

    #[test]
    fn armed_survives_pullback_via_high_water() {
        let high_water = 106.0;
        assert!(armed(100.0, high_water, 5.0, true));
    }

    #[test]
    fn lock_price_mirrors_by_side() {
        assert_eq!(lock_price(100.0, 1.0, true), 101.0);
        assert_eq!(lock_price(100.0, 1.0, false), 99.0);
    }
}
