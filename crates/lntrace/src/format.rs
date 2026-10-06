/// Format a fee in millisatoshis as a human-readable sat string.
///
/// - 0 msat → "0"
/// - 5000 msat → "5" (exact whole sat)
/// - 54499 msat → "≈54.5" (rounded to 1 decimal)
/// - 999 msat → "≈1" (fractional that rounds to whole)
pub fn format_sat_fee(msat: u64) -> String {
    if msat.is_multiple_of(1000) {
        return (msat / 1000).to_string();
    }
    let sat = msat as f64 / 1000.0;
    let rounded = (sat * 10.0).round() / 10.0;
    if rounded == rounded.floor() {
        format!("≈{}", rounded as u64)
    } else {
        format!("≈{rounded:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_msat() {
        assert_eq!(format_sat_fee(0), "0");
    }

    #[test]
    fn exact_whole_sat() {
        assert_eq!(format_sat_fee(5000), "5");
        assert_eq!(format_sat_fee(100_000), "100");
    }

    #[test]
    fn fractional_rounds_to_one_decimal() {
        assert_eq!(format_sat_fee(54_499), "≈54.5");
        assert_eq!(format_sat_fee(501), "≈0.5");
        assert_eq!(format_sat_fee(1500), "≈1.5");
    }

    #[test]
    fn fractional_that_rounds_to_whole() {
        assert_eq!(format_sat_fee(999), "≈1");
        assert_eq!(format_sat_fee(50_050), "≈50.1");
    }
}
