pub(crate) fn rid_to_string(rid: u64) -> String {
    format!("r{rid}")
}

pub(crate) fn parse_rid(id: &str) -> Option<u64> {
    let trimmed = id.trim();
    let numeric = trimmed
        .strip_prefix('r')
        .or_else(|| trimmed.strip_prefix('R'))?;
    numeric.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn rid_roundtrips() {
        let id = rid_to_string(42);
        assert_eq!(id, "r42");
        assert_eq!(parse_rid(&id), Some(42));
        assert_eq!(parse_rid("R42"), Some(42));
        assert_eq!(parse_rid("x42"), None);
    }
}
