/// Hard limits every outgoing action must pass. Constructed from config once;
/// there is no code path that bypasses them.
#[derive(Clone, Debug)]
pub struct SafetyLimits {
    pub max_order_notional: f64,
    pub min_action_interval_ms: u64,
}

impl SafetyLimits {
    pub fn new(max_order_notional: f64, min_action_interval_ms: u64) -> Self {
        Self {
            max_order_notional,
            min_action_interval_ms,
        }
    }

    /// Backstop against malformed feeds: zero, negative, and non-finite marks
    /// are never valid prices. A rolling deviation band is added in the
    /// implementation pass.
    pub fn price_is_sane(mark: f64) -> bool {
        mark.is_finite() && mark > 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_and_non_finite_marks() {
        assert!(!SafetyLimits::price_is_sane(0.0));
        assert!(!SafetyLimits::price_is_sane(-1.0));
        assert!(!SafetyLimits::price_is_sane(f64::NAN));
        assert!(!SafetyLimits::price_is_sane(f64::INFINITY));
        assert!(SafetyLimits::price_is_sane(60_000.0));
    }
}
