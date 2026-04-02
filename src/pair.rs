pub fn canonicalize_pair(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_sep = false;

    for ch in input.chars() {
        let normalized = ch.to_ascii_uppercase();
        if normalized.is_ascii_alphanumeric() {
            out.push(normalized);
            prev_sep = false;
        } else if !prev_sep && !out.is_empty() {
            out.push('_');
            prev_sep = true;
        }
    }

    out.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::canonicalize_pair;

    #[test]
    fn canonicalization_works() {
        assert_eq!(canonicalize_pair("btc_usd"), "BTC_USD");
        assert_eq!(canonicalize_pair("BTC-USD"), "BTC_USD");
        assert_eq!(canonicalize_pair("BTC / USD"), "BTC_USD");
        assert_eq!(canonicalize_pair("  sUSDe / usd  "), "SUSDE_USD");
    }
}
