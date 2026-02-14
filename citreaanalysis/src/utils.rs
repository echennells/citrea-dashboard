use crate::error::Result;

/// Parse hexadecimal string (with optional "0x" prefix) to u64.
pub fn parse_hex_u64(hex_str: &str) -> Result<u64> {
    let trimmed = hex_str.trim_start_matches("0x");
    Ok(u64::from_str_radix(trimmed, 16)?)
}

/// Parse hex to u64, returning default value on failure.
pub fn parse_hex_u64_or_default(hex_str: &str, default: u64) -> u64 {
    parse_hex_u64(hex_str).unwrap_or(default)
}

/// Parse hex to u64, returning None on failure.
pub fn parse_hex_u64_opt(hex_str: &str) -> Option<u64> {
    parse_hex_u64(hex_str).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hex_u64() {
        assert_eq!(parse_hex_u64("0x10").unwrap(), 16);
        assert_eq!(parse_hex_u64("10").unwrap(), 16);
        assert_eq!(parse_hex_u64("0xff").unwrap(), 255);
        assert!(parse_hex_u64("invalid").is_err());
    }

    #[test]
    fn test_parse_hex_u64_or_default() {
        assert_eq!(parse_hex_u64_or_default("0x10", 999), 16);
        assert_eq!(parse_hex_u64_or_default("invalid", 999), 999);
    }

    #[test]
    fn test_parse_hex_u64_opt() {
        assert_eq!(parse_hex_u64_opt("0x10"), Some(16));
        assert_eq!(parse_hex_u64_opt("invalid"), None);
    }
}
