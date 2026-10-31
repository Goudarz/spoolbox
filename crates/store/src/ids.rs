//! ULID-style message IDs: 48-bit millisecond time + 80 random bits, Crockford base32.

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

pub(crate) fn new_id(now_micros: i64) -> String {
    let ms = (now_micros / 1000).max(0) as u128 & ((1 << 48) - 1);
    let mut random = [0u8; 10];
    if getrandom::fill(&mut random).is_err() {
        // Uniqueness is still guaranteed by the database constraint; this only weakens unpredictability.
        random = (now_micros as u128).to_le_bytes()[..10]
            .try_into()
            .unwrap_or_default();
    }
    let value = (ms << 80)
        | random
            .iter()
            .fold(0u128, |acc, b| (acc << 8) | u128::from(*b));
    (0..26)
        .rev()
        .map(|i| ALPHABET[((value >> (i * 5)) & 31) as usize] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_26_chars_sortable_and_unique() {
        let a = new_id(1_000_000);
        let b = new_id(2_000_000);
        assert_eq!(a.len(), 26);
        assert!(a < b, "time-ordered");
        assert_ne!(new_id(5), new_id(5));
        assert!(a.chars().all(|c| ALPHABET.contains(&(c as u8))));
    }
}
