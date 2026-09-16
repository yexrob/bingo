//! One spelling of a byte count for a model. The kernel says how heavy a
//! picture it elided was (ADR-0061) and the `Read` tool says how heavy the
//! file behind a bounded one is (ADR-0062 §3); two crates saying the same
//! number two ways would be two representations of one fact.

/// Decimal units, one decimal: what a picture costs the wire is bytes, and
/// bytes are what a download says. Rounded before the unit is chosen, so a
/// picture just under a megabyte reads `1.0 MB` and never `1000.0 KB`.
pub fn words(bytes: usize) -> String {
    let tenths_of_kb = bytes.saturating_add(50) / 100;
    if tenths_of_kb < 10_000 {
        return format!("{}.{} KB", tenths_of_kb / 10, tenths_of_kb % 10);
    }
    let tenths_of_mb = bytes.saturating_add(50_000) / 100_000;
    format!("{}.{} MB", tenths_of_mb / 10, tenths_of_mb % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_size_is_decimal_with_one_decimal_place() {
        assert_eq!(words(0), "0.0 KB");
        assert_eq!(words(949), "0.9 KB");
        assert_eq!(words(950), "1.0 KB");
        assert_eq!(words(3_000), "3.0 KB");
        assert_eq!(words(999_949), "999.9 KB");
        assert_eq!(words(999_950), "1.0 MB");
        assert_eq!(words(1_000_000), "1.0 MB");
        assert_eq!(words(2_212_534), "2.2 MB");
        assert!(
            words(usize::MAX).ends_with(" MB"),
            "the arithmetic saturates"
        );
    }
}
