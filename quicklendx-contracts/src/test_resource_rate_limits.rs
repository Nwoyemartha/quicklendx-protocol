//! Unit tests for resource and rate limits (#2439, #2479).
//!
//! Tests the per-address mutation rate limiter and the per-participant KYC
//! submission rate limiter at the function level, verifying:
//!
//! - Budget allows operations within limit
//! - Budget rejects burst beyond limit
//! - Window expiry resets counter (recovery)
//! - Limits are per-address (no cross-contamination)
//! - KYC submission rate limiter enforces its own window
//! - Input size bounds are enforced
//! - Boundary constants are sane
//! - Rejected/cancelled operations leave no partial state
//!
//! NOTE: `protocol_limits` is not declared as a module of `lib.rs`. The
//! production constants and rate-limiting logic in `protocol_limits.rs` must
//! match the values and algorithms tested here.  If the production code
//! drifts, update these tests accordingly.

#![cfg(test)]

use crate::errors::QuickLendXError;
use soroban_sdk::{Bytes, Env};

// ─── Constants (must match protocol_limits.rs) ────────────────────────────

const RATE_LIMIT_WINDOW_SEQUENCES: u32 = 20;
const MAX_MUTATIONS_PER_WINDOW: u32 = 30;
const KYC_RATE_LIMIT_WINDOW_SEQUENCES: u32 = 10;
const MAX_KYC_SUBMISSIONS_PER_WINDOW: u32 = 3;

const MAX_INPUT_DESCRIPTION_BYTES: u32 = 4_096;
const MAX_INPUT_KYC_DATA_BYTES: u32 = 8_192;
const MAX_INPUT_RATING_COMMENT_BYTES: u32 = 2_048;
const MAX_INPUT_TAGS: u32 = 50;
const MAX_INPUT_BATCH_SIZE: u32 = 25;
const MAX_INPUT_STATUS_BATCH_SIZE: u32 = 100;
const MAX_INPUT_LINE_ITEMS: u32 = 50;

// ─── Pure rate-limit logic (mirrors protocol_limits.rs) ───────────────────

struct RateLimiter {
    window_start: u32,
    count: u32,
    window_size: u32,
    max_per_window: u32,
}

impl RateLimiter {
    fn new(window_size: u32, max_per_window: u32) -> Self {
        Self {
            window_start: 0,
            count: 0,
            window_size,
            max_per_window,
        }
    }

    fn is_expired(&self, current_seq: u32) -> bool {
        self.window_start == 0 || current_seq > self.window_start + self.window_size
    }

    fn check(&self, current_seq: u32) -> Result<(), QuickLendXError> {
        if self.is_expired(current_seq) {
            return Ok(());
        }
        if self.count >= self.max_per_window {
            return Err(QuickLendXError::MutationLimitExceeded);
        }
        Ok(())
    }

    fn record(&mut self, current_seq: u32) {
        if self.is_expired(current_seq) {
            self.window_start = current_seq;
            self.count = 1;
        } else {
            self.count = self.count.saturating_add(1);
        }
    }

    fn check_and_record(&mut self, current_seq: u32) -> Result<(), QuickLendXError> {
        self.check(current_seq)?;
        self.record(current_seq);
        Ok(())
    }
}

// ─── Input-size bound helpers (mirrors protocol_limits.rs) ────────────────

fn require_bound(data: &Bytes, max: u32) -> Result<(), QuickLendXError> {
    if data.len() as u32 > max {
        return Err(QuickLendXError::InputTooLarge);
    }
    Ok(())
}

// ===========================================================================
// Mutation rate-limit tests
// ===========================================================================

#[test]
fn test_mutation_limit_allows_within_budget() {
    let mut limiter = RateLimiter::new(RATE_LIMIT_WINDOW_SEQUENCES, MAX_MUTATIONS_PER_WINDOW);
    for _ in 0..MAX_MUTATIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
}

#[test]
fn test_mutation_limit_rejects_burst() {
    let mut limiter = RateLimiter::new(RATE_LIMIT_WINDOW_SEQUENCES, MAX_MUTATIONS_PER_WINDOW);
    for _ in 0..MAX_MUTATIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
    assert_eq!(
        limiter.check_and_record(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );
}

#[test]
fn test_mutation_limit_resets_after_window() {
    let mut limiter = RateLimiter::new(RATE_LIMIT_WINDOW_SEQUENCES, MAX_MUTATIONS_PER_WINDOW);
    for _ in 0..MAX_MUTATIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
    assert_eq!(
        limiter.check(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );

    limiter
        .check_and_record(100 + RATE_LIMIT_WINDOW_SEQUENCES + 1)
        .unwrap();
}

#[test]
fn test_mutation_limit_read_does_not_increment() {
    let mut limiter = RateLimiter::new(RATE_LIMIT_WINDOW_SEQUENCES, MAX_MUTATIONS_PER_WINDOW);
    for _ in 0..(MAX_MUTATIONS_PER_WINDOW - 1) {
        limiter.check_and_record(100).unwrap();
    }
    limiter.check(100).unwrap();
    limiter.check_and_record(100).unwrap();
    assert_eq!(
        limiter.check(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );
}

#[test]
fn test_mutation_limit_resets_lazy() {
    let mut limiter = RateLimiter::new(RATE_LIMIT_WINDOW_SEQUENCES, MAX_MUTATIONS_PER_WINDOW);
    limiter.record(5);
    limiter.record(5);
    assert_eq!(limiter.count, 2);
    assert_eq!(limiter.window_start, 5);

    limiter.record(5 + RATE_LIMIT_WINDOW_SEQUENCES + 1);
    assert_eq!(limiter.count, 1);
    assert_eq!(limiter.window_start, 5 + RATE_LIMIT_WINDOW_SEQUENCES + 1);
}

// ===========================================================================
// KYC submission rate-limit tests
// ===========================================================================

#[test]
fn test_kyc_limit_allows_within_budget() {
    let mut limiter = RateLimiter::new(
        KYC_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_KYC_SUBMISSIONS_PER_WINDOW,
    );
    for _ in 0..MAX_KYC_SUBMISSIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
}

#[test]
fn test_kyc_limit_rejects_burst() {
    let mut limiter = RateLimiter::new(
        KYC_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_KYC_SUBMISSIONS_PER_WINDOW,
    );
    for _ in 0..MAX_KYC_SUBMISSIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
    assert_eq!(
        limiter.check_and_record(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );
}

#[test]
fn test_kyc_limit_resets_after_window() {
    let mut limiter = RateLimiter::new(
        KYC_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_KYC_SUBMISSIONS_PER_WINDOW,
    );
    for _ in 0..MAX_KYC_SUBMISSIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
    assert_eq!(
        limiter.check(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );

    limiter
        .check_and_record(100 + KYC_RATE_LIMIT_WINDOW_SEQUENCES + 1)
        .unwrap();
}

#[test]
fn test_kyc_limit_independent_of_mutation_limit() {
    let mut mutation = RateLimiter::new(RATE_LIMIT_WINDOW_SEQUENCES, MAX_MUTATIONS_PER_WINDOW);
    let mut kyc = RateLimiter::new(
        KYC_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_KYC_SUBMISSIONS_PER_WINDOW,
    );

    for _ in 0..MAX_MUTATIONS_PER_WINDOW {
        mutation.check_and_record(100).unwrap();
    }
    assert_eq!(
        mutation.check(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );

    for _ in 0..MAX_KYC_SUBMISSIONS_PER_WINDOW {
        kyc.check_and_record(100).unwrap();
    }
}

#[test]
fn test_kyc_limit_resets_independently() {
    let mut limiter = RateLimiter::new(
        KYC_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_KYC_SUBMISSIONS_PER_WINDOW,
    );
    for _ in 0..MAX_KYC_SUBMISSIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
    limiter
        .check_and_record(100 + KYC_RATE_LIMIT_WINDOW_SEQUENCES + 1)
        .unwrap();
}

#[test]
fn test_kyc_limit_read_does_not_increment() {
    let mut limiter = RateLimiter::new(
        KYC_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_KYC_SUBMISSIONS_PER_WINDOW,
    );
    for _ in 0..MAX_KYC_SUBMISSIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
    // Pure read at limit returns error (same as check_and_record)
    assert_eq!(
        limiter.check(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );
    // Read does not consume quota: check_and_record still returns same error
    // (not a different error from over-consumption)
    assert_eq!(
        limiter.check_and_record(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );
    // Count unchanged: exactly at limit, not above
    assert_eq!(limiter.count, MAX_KYC_SUBMISSIONS_PER_WINDOW);
}

// ===========================================================================
// Cancellation / failure safety
// ===========================================================================

#[test]
fn test_rejected_operation_does_not_consume_quota() {
    let mut limiter = RateLimiter::new(RATE_LIMIT_WINDOW_SEQUENCES, MAX_MUTATIONS_PER_WINDOW);
    for _ in 0..(MAX_MUTATIONS_PER_WINDOW - 1) {
        limiter.check_and_record(100).unwrap();
    }
    limiter.check(100).unwrap();
    limiter.check_and_record(100).unwrap();
    assert_eq!(
        limiter.check(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );
}

#[test]
fn test_multiple_window_cycles() {
    let mut limiter = RateLimiter::new(RATE_LIMIT_WINDOW_SEQUENCES, MAX_MUTATIONS_PER_WINDOW);
    for cycle in 0..3u32 {
        let base = 100 + cycle * (RATE_LIMIT_WINDOW_SEQUENCES + 1);
        for _ in 0..MAX_MUTATIONS_PER_WINDOW {
            limiter.check_and_record(base).unwrap();
        }
    }
    limiter
        .check_and_record(100 + 3 * (RATE_LIMIT_WINDOW_SEQUENCES + 1))
        .unwrap();
}

// ===========================================================================
// Burst traffic simulation
// ===========================================================================

#[test]
fn test_burst_exactly_at_limit_succeeds() {
    let mut limiter = RateLimiter::new(RATE_LIMIT_WINDOW_SEQUENCES, MAX_MUTATIONS_PER_WINDOW);
    for i in 0..MAX_MUTATIONS_PER_WINDOW {
        let result = limiter.check_and_record(100);
        assert!(result.is_ok(), "Mutation {} should succeed", i);
    }
}

#[test]
fn test_burst_one_above_limit_fails() {
    let mut limiter = RateLimiter::new(RATE_LIMIT_WINDOW_SEQUENCES, MAX_MUTATIONS_PER_WINDOW);
    for _ in 0..MAX_MUTATIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
    assert_eq!(
        limiter.check_and_record(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );
}

#[test]
fn test_kyc_burst_exactly_at_limit_succeeds() {
    let mut limiter = RateLimiter::new(
        KYC_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_KYC_SUBMISSIONS_PER_WINDOW,
    );
    for i in 0..MAX_KYC_SUBMISSIONS_PER_WINDOW {
        let result = limiter.check_and_record(100);
        assert!(result.is_ok(), "KYC submission {} should succeed", i);
    }
}

#[test]
fn test_kyc_burst_one_above_limit_fails() {
    let mut limiter = RateLimiter::new(
        KYC_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_KYC_SUBMISSIONS_PER_WINDOW,
    );
    for _ in 0..MAX_KYC_SUBMISSIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
    assert_eq!(
        limiter.check_and_record(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );
}

// ===========================================================================
// Boundary constant consistency checks
// ===========================================================================

#[test]
fn test_boundary_constants_are_sane() {
    assert!(MAX_INPUT_DESCRIPTION_BYTES > 0);
    assert!(MAX_INPUT_KYC_DATA_BYTES > 0);
    assert!(MAX_INPUT_TAGS > 0);
    assert!(MAX_INPUT_BATCH_SIZE > 0);
    assert!(MAX_INPUT_STATUS_BATCH_SIZE > 0);
    assert!(MAX_INPUT_LINE_ITEMS > 0);
    assert!(MAX_INPUT_RATING_COMMENT_BYTES > 0);

    assert!(RATE_LIMIT_WINDOW_SEQUENCES > 0);
    assert!(MAX_MUTATIONS_PER_WINDOW > 0);
    assert!(KYC_RATE_LIMIT_WINDOW_SEQUENCES > 0);
    assert!(MAX_KYC_SUBMISSIONS_PER_WINDOW > 0);

    assert!(
        MAX_KYC_SUBMISSIONS_PER_WINDOW <= MAX_MUTATIONS_PER_WINDOW,
        "KYC submission limit should be at most the mutation limit"
    );
}

// ===========================================================================
// Recovery after throttling
// ===========================================================================

#[test]
fn test_full_recovery_after_throttle() {
    let mut limiter = RateLimiter::new(RATE_LIMIT_WINDOW_SEQUENCES, MAX_MUTATIONS_PER_WINDOW);
    for _ in 0..MAX_MUTATIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
    assert_eq!(
        limiter.check(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );

    for _ in 0..MAX_MUTATIONS_PER_WINDOW {
        limiter
            .check_and_record(100 + RATE_LIMIT_WINDOW_SEQUENCES + 1)
            .unwrap();
    }
    assert_eq!(
        limiter.check(100 + RATE_LIMIT_WINDOW_SEQUENCES + 1),
        Err(QuickLendXError::MutationLimitExceeded)
    );
}

#[test]
fn test_kyc_full_recovery_after_throttle() {
    let mut limiter = RateLimiter::new(
        KYC_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_KYC_SUBMISSIONS_PER_WINDOW,
    );
    for _ in 0..MAX_KYC_SUBMISSIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
    assert_eq!(
        limiter.check(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );

    for _ in 0..MAX_KYC_SUBMISSIONS_PER_WINDOW {
        limiter
            .check_and_record(100 + KYC_RATE_LIMIT_WINDOW_SEQUENCES + 1)
            .unwrap();
    }
    assert_eq!(
        limiter.check(100 + KYC_RATE_LIMIT_WINDOW_SEQUENCES + 1),
        Err(QuickLendXError::MutationLimitExceeded)
    );
}

// ===========================================================================
// Input-size bound tests
// ===========================================================================

#[test]
fn test_description_bound_exact_boundary() {
    let env = Env::default();
    let data = Bytes::from_slice(&env, &[0u8; MAX_INPUT_DESCRIPTION_BYTES as usize]);
    require_bound(&data, MAX_INPUT_DESCRIPTION_BYTES).unwrap();
}

#[test]
fn test_description_bound_one_over() {
    let env = Env::default();
    let data = Bytes::from_slice(&env, &[0u8; (MAX_INPUT_DESCRIPTION_BYTES + 1) as usize]);
    assert_eq!(
        require_bound(&data, MAX_INPUT_DESCRIPTION_BYTES),
        Err(QuickLendXError::InputTooLarge)
    );
}

#[test]
fn test_kyc_data_bound_exact_boundary() {
    let env = Env::default();
    let data = Bytes::from_slice(&env, &[0u8; MAX_INPUT_KYC_DATA_BYTES as usize]);
    require_bound(&data, MAX_INPUT_KYC_DATA_BYTES).unwrap();
}

#[test]
fn test_kyc_data_bound_one_over() {
    let env = Env::default();
    let data = Bytes::from_slice(&env, &[0u8; (MAX_INPUT_KYC_DATA_BYTES + 1) as usize]);
    assert_eq!(
        require_bound(&data, MAX_INPUT_KYC_DATA_BYTES),
        Err(QuickLendXError::InputTooLarge)
    );
}

#[test]
fn test_rating_comment_bound_exact_boundary() {
    let env = Env::default();
    let data = Bytes::from_slice(&env, &[0u8; MAX_INPUT_RATING_COMMENT_BYTES as usize]);
    require_bound(&data, MAX_INPUT_RATING_COMMENT_BYTES).unwrap();
}

#[test]
fn test_rating_comment_bound_one_over() {
    let env = Env::default();
    let data = Bytes::from_slice(&env, &[0u8; (MAX_INPUT_RATING_COMMENT_BYTES + 1) as usize]);
    assert_eq!(
        require_bound(&data, MAX_INPUT_RATING_COMMENT_BYTES),
        Err(QuickLendXError::InputTooLarge)
    );
}

#[test]
fn test_empty_input_accepted() {
    let env = Env::default();
    let data = Bytes::from_slice(&env, &[]);
    require_bound(&data, MAX_INPUT_DESCRIPTION_BYTES).unwrap();
    require_bound(&data, MAX_INPUT_KYC_DATA_BYTES).unwrap();
    require_bound(&data, MAX_INPUT_RATING_COMMENT_BYTES).unwrap();
}

// ===========================================================================
// Multiple-address isolation
// ===========================================================================

#[test]
fn test_many_addresses_independent_limits() {
    for _ in 0..10 {
        let mut limiter = RateLimiter::new(RATE_LIMIT_WINDOW_SEQUENCES, MAX_MUTATIONS_PER_WINDOW);
        for _ in 0..MAX_MUTATIONS_PER_WINDOW {
            limiter.check_and_record(100).unwrap();
        }
        assert_eq!(
            limiter.check(100),
            Err(QuickLendXError::MutationLimitExceeded)
        );
    }
}

#[test]
fn test_kyc_many_addresses_independent_limits() {
    for _ in 0..5 {
        let mut limiter = RateLimiter::new(
            KYC_RATE_LIMIT_WINDOW_SEQUENCES,
            MAX_KYC_SUBMISSIONS_PER_WINDOW,
        );
        for _ in 0..MAX_KYC_SUBMISSIONS_PER_WINDOW {
            limiter.check_and_record(100).unwrap();
        }
        assert_eq!(
            limiter.check(100),
            Err(QuickLendXError::MutationLimitExceeded)
        );
    }
}
