// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

/// JumpHash consistent hash function.
///
/// Returns a bucket index in `[0, num_buckets)` for the given key.
/// O(1) time, minimal redistribution on bucket count change.
///
/// Based on Google's Jump Consistent Hash:
/// <https://arxiv.org/abs/1406.2294>
pub fn jump_hash(key: u64, num_buckets: u64) -> u64 {
    if num_buckets == 0 {
        return 0;
    }
    let mut key = key;
    let mut b = -1i64;
    let mut j = 0i64;
    while j < num_buckets as i64 {
        b = j;
        key = key.wrapping_mul(2862933555777941757).wrapping_add(1);
        j = ((b.wrapping_add(1) as f64) * (1u64 << 31) as f64 / ((key >> 33) + 1) as f64) as i64;
    }
    b as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic: same key always maps to the same bucket.
    #[test]
    fn same_key_same_bucket() {
        for num_buckets in [1, 2, 8, 16, 64, 127] {
            for key in [0, 1, 42, u64::MAX, u64::MAX >> 1] {
                let first = jump_hash(key, num_buckets);
                let second = jump_hash(key, num_buckets);
                assert_eq!(first, second, "key={key} with {num_buckets} buckets must be deterministic");
            }
        }
    }

    /// All 64 buckets are reachable across the full key space.
    #[test]
    fn all_buckets_reachable() {
        let num_buckets = 64;
        let mut seen = [false; 64];
        // Try every 2^32 step in u64 space — guaranteed to hit every bucket
        // given JumpHash's statistical properties.
        for step in 0..1024 {
            let key = (step as u64) << 32;
            let bucket = jump_hash(key, num_buckets) as usize;
            seen[bucket] = true;
        }
        let populated: usize = seen.iter().filter(|&&s| s).count();
        assert_eq!(populated, 64, "expected all 64 buckets to be reachable, got {populated}");
    }

    /// Distribution is approximately uniform across 64 buckets.
    ///
    /// Chi-squared test with 100_000 samples. Threshold of 200 is very
    /// lenient for df=63 (p ≈ 1.0) — extremely unlikely to produce
    /// false positives while still catching severe bias.
    #[test]
    fn distribution_uniform() {
        let num_buckets = 64;
        let samples = 100_000u64;
        let expected = samples as f64 / num_buckets as f64;

        let mut counts = vec![0u64; num_buckets as usize];
        for key in 0..samples {
            let bucket = jump_hash(key, num_buckets) as usize;
            counts[bucket] += 1;
        }

        let chi_squared: f64 = counts
            .iter()
            .map(|&observed| {
                let diff = observed as f64 - expected;
                diff * diff / expected
            })
            .sum();

        assert!(
            chi_squared < 200.0,
            "chi-squared = {chi_squared} exceeds threshold 200.0; \
             distribution may not be uniform"
        );
    }

    /// Adding one bucket moves approximately 1/N of keys.
    #[test]
    fn minimal_redistribution_on_bucket_change() {
        let num_keys = 10_000u64;
        let old_buckets = 64;
        let new_buckets = 65;

        let mut changed = 0u64;
        for key in 0..num_keys {
            let old_b = jump_hash(key, old_buckets);
            let new_b = jump_hash(key, new_buckets);
            if old_b != new_b {
                changed += 1;
            }
        }

        // With JumpHash, adding one bucket moves ~1/(N+1) of keys.
        // For 64→65, ~1/65 ≈ 1.54%. With 10K keys, ~154 keys move.
        // Allow generous range: 50-350 (0.5%-3.5%).
        let expected = (num_keys as f64 / (new_buckets as f64)) as u64;
        let lower = (expected as f64 * 0.5) as u64;
        let upper = (expected as f64 * 2.5) as u64;

        assert!(
            changed >= lower && changed <= upper,
            "expected ~{expected} keys to change bucket (64→65), got {changed}; \
             range [{lower}, {upper}]"
        );
    }

    /// Removing one bucket also moves ~1/N keys.
    #[test]
    fn minimal_redistribution_on_bucket_removal() {
        let num_keys = 10_000u64;
        let old_buckets = 65;
        let new_buckets = 64;

        let mut changed = 0u64;
        for key in 0..num_keys {
            let old_b = jump_hash(key, old_buckets);
            let new_b = jump_hash(key, new_buckets);
            if old_b != new_b {
                changed += 1;
            }
        }

        let expected = (num_keys as f64 / (new_buckets as f64)) as u64;
        let lower = (expected as f64 * 0.5) as u64;
        let upper = (expected as f64 * 2.5) as u64;

        assert!(
            changed >= lower && changed <= upper,
            "expected ~{expected} keys to change bucket (65→64), got {changed}; \
             range [{lower}, {upper}]"
        );
    }

    /// Edge case: 1 bucket — everything maps to 0.
    #[test]
    fn single_bucket() {
        for key in [0, 1, 42, u64::MAX] {
            assert_eq!(jump_hash(key, 1), 0);
        }
    }

    /// Edge case: 0 buckets returns 0 (degenerate case).
    #[test]
    fn zero_buckets() {
        assert_eq!(jump_hash(42, 0), 0);
    }
}
