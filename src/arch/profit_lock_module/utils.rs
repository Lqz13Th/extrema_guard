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
