//! A tiny stable hash (FNV-1a) for ids and cache keys; not for security.

pub fn fnv64<'a>(parts: impl IntoIterator<Item = &'a str>) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        // The separator keeps ("ab", "c") apart from ("a", "bc").
        for byte in part.bytes().chain(std::iter::once(0x1f)) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}
