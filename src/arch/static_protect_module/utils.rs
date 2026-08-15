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
