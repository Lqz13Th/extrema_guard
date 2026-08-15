use std::{env::current_dir, fs};

use serde::Deserialize;

use extrema_infra::prelude::*;

#[derive(Clone, Debug, Deserialize)]
pub struct StaticProtectConfig {
    #[serde(default)]
    pub enabled: bool,
    pub schedule_duration_sec: u64,
    pub schedule_task_id: u64,
    #[serde(default)]
    pub rules: Vec<StaticRule>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct StaticRule {
    pub inst: String,
    pub sl_pct: Option<f64>,
    pub sl_risk: Option<f64>,
    pub tp_pct: Option<f64>,
    pub tp_risk: Option<f64>,
}

#[derive(Deserialize)]
struct StrategyConfigToml {
    static_protect: StaticProtectConfig,
}

impl StaticProtectConfig {
    pub fn validate(&self) -> InfraResult<()> {
        if self.schedule_duration_sec == 0 {
            return Err(InfraError::Msg(
                "static_protect.schedule_duration_sec must be positive".to_string(),
            ));
        }
        for rule in &self.rules {
            rule.validate()?;
        }
        Ok(())
    }
}

impl StaticRule {
    fn validate(&self) -> InfraResult<()> {
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

pub fn load_static_protect_config() -> InfraResult<StaticProtectConfig> {
    let path = current_dir()
        .map_err(InfraError::Io)?
        .join("strategy_config.toml");
    let raw = fs::read_to_string(&path)
        .map_err(|err| InfraError::Msg(format!("read {}: {err}", path.display())))?;
    let wrapper: StrategyConfigToml = toml::from_str(&raw)
        .map_err(|err| InfraError::Msg(format!("parse {}: {err}", path.display())))?;
    wrapper.static_protect.validate()?;
    Ok(wrapper.static_protect)
}

pub fn offset_from_pct(entry: f64, pct: f64) -> f64 {
    entry * pct / 100.0
}

pub fn offset_from_risk(risk: f64, size_abs: f64) -> Option<f64> {
    (size_abs > 0.0).then(|| risk / size_abs)
}

pub fn stop_price(entry: f64, offset: f64, is_long: bool) -> f64 {
    if is_long {
        entry - offset
    } else {
        entry + offset
    }
}

pub fn target_price(entry: f64, offset: f64, is_long: bool) -> f64 {
    if is_long {
        entry + offset
    } else {
        entry - offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pct_offset_scales_with_entry() {
        assert_eq!(offset_from_pct(60_000.0, 3.0), 1_800.0);
    }

    #[test]
    fn risk_offset_normalizes_by_position_size() {
        assert_eq!(offset_from_risk(10.0, 0.5), Some(20.0));
        assert_eq!(offset_from_risk(10.0, 0.0), None);
    }

    #[test]
    fn stop_and_target_mirror_by_side() {
        assert_eq!(stop_price(100.0, 3.0, true), 97.0);
        assert_eq!(stop_price(100.0, 3.0, false), 103.0);
        assert_eq!(target_price(100.0, 3.0, true), 103.0);
        assert_eq!(target_price(100.0, 3.0, false), 97.0);
    }
}
