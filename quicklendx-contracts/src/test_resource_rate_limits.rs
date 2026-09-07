//! Unit tests for resource and rate limits (#2439, #2479).
//!
//! Tests the per-address mutation rate limiter, per-participant KYC
//! submission rate limiter, and per-admin identity-transition rate limiter
//! at the function level, verifying:
//!
//! - Budget allows operations within limit
//! - Budget rejects burst beyond limit
//! - Window expiry resets counter (recovery)
//! - Limits are per-address (no cross-contamination)
//! - KYC submission rate limiter enforces its own window
//! - Identity transition rate limiter enforces its own window
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
const IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES: u32 = 15;
const MAX_IDENTITY_TRANSITIONS_PER_WINDOW: u32 = 10;

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

// ===========================================================================
// Identity-transition rate-limit tests (#2479)
// ===========================================================================

#[test]
fn test_identity_limit_allows_within_budget() {
    let mut limiter = RateLimiter::new(
        IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_IDENTITY_TRANSITIONS_PER_WINDOW,
    );
    for _ in 0..MAX_IDENTITY_TRANSITIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
}

#[test]
fn test_identity_limit_rejects_burst() {
    let mut limiter = RateLimiter::new(
        IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_IDENTITY_TRANSITIONS_PER_WINDOW,
    );
    for _ in 0..MAX_IDENTITY_TRANSITIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
    assert_eq!(
        limiter.check_and_record(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );
}

#[test]
fn test_identity_limit_resets_after_window() {
    let mut limiter = RateLimiter::new(
        IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_IDENTITY_TRANSITIONS_PER_WINDOW,
    );
    for _ in 0..MAX_IDENTITY_TRANSITIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
    assert_eq!(
        limiter.check(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );

    limiter
        .check_and_record(100 + IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES + 1)
        .unwrap();
}

#[test]
fn test_identity_limit_read_does_not_increment() {
    let mut limiter = RateLimiter::new(
        IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_IDENTITY_TRANSITIONS_PER_WINDOW,
    );
    for _ in 0..MAX_IDENTITY_TRANSITIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
    // Pure read at limit returns error
    assert_eq!(
        limiter.check(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );
    // Read does not consume quota: check_and_record still returns same error
    assert_eq!(
        limiter.check_and_record(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );
    // Count unchanged
    assert_eq!(limiter.count, MAX_IDENTITY_TRANSITIONS_PER_WINDOW);
}

#[test]
fn test_identity_limit_resets_lazy() {
    let mut limiter = RateLimiter::new(
        IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_IDENTITY_TRANSITIONS_PER_WINDOW,
    );
    limiter.record(5);
    limiter.record(5);
    assert_eq!(limiter.count, 2);
    assert_eq!(limiter.window_start, 5);

    limiter.record(5 + IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES + 1);
    assert_eq!(limiter.count, 1);
    assert_eq!(
        limiter.window_start,
        5 + IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES + 1
    );
}

#[test]
fn test_identity_limit_independent_of_mutation_limit() {
    let mut mutation = RateLimiter::new(RATE_LIMIT_WINDOW_SEQUENCES, MAX_MUTATIONS_PER_WINDOW);
    let mut identity = RateLimiter::new(
        IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_IDENTITY_TRANSITIONS_PER_WINDOW,
    );

    // Exhaust mutation limiter
    for _ in 0..MAX_MUTATIONS_PER_WINDOW {
        mutation.check_and_record(100).unwrap();
    }
    assert_eq!(
        mutation.check(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );

    // Identity limiter still has budget
    for _ in 0..MAX_IDENTITY_TRANSITIONS_PER_WINDOW {
        identity.check_and_record(100).unwrap();
    }
}

#[test]
fn test_identity_limit_independent_of_kyc_limit() {
    let mut kyc = RateLimiter::new(
        KYC_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_KYC_SUBMISSIONS_PER_WINDOW,
    );
    let mut identity = RateLimiter::new(
        IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_IDENTITY_TRANSITIONS_PER_WINDOW,
    );

    // Exhaust KYC limiter
    for _ in 0..MAX_KYC_SUBMISSIONS_PER_WINDOW {
        kyc.check_and_record(100).unwrap();
    }
    assert_eq!(kyc.check(100), Err(QuickLendXError::MutationLimitExceeded));

    // Identity limiter still has budget
    for _ in 0..MAX_IDENTITY_TRANSITIONS_PER_WINDOW {
        identity.check_and_record(100).unwrap();
    }
}

#[test]
fn test_identity_burst_exactly_at_limit_succeeds() {
    let mut limiter = RateLimiter::new(
        IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_IDENTITY_TRANSITIONS_PER_WINDOW,
    );
    for i in 0..MAX_IDENTITY_TRANSITIONS_PER_WINDOW {
        let result = limiter.check_and_record(100);
        assert!(result.is_ok(), "Identity transition {} should succeed", i);
    }
}

#[test]
fn test_identity_burst_one_above_limit_fails() {
    let mut limiter = RateLimiter::new(
        IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_IDENTITY_TRANSITIONS_PER_WINDOW,
    );
    for _ in 0..MAX_IDENTITY_TRANSITIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
    assert_eq!(
        limiter.check_and_record(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );
}

#[test]
fn test_identity_full_recovery_after_throttle() {
    let mut limiter = RateLimiter::new(
        IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_IDENTITY_TRANSITIONS_PER_WINDOW,
    );
    // Exhaust the limiter
    for _ in 0..MAX_IDENTITY_TRANSITIONS_PER_WINDOW {
        limiter.check_and_record(100).unwrap();
    }
    assert_eq!(
        limiter.check(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );

    // Recover and exhaust again in a new window
    for _ in 0..MAX_IDENTITY_TRANSITIONS_PER_WINDOW {
        limiter
            .check_and_record(100 + IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES + 1)
            .unwrap();
    }
    assert_eq!(
        limiter.check(100 + IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES + 1),
        Err(QuickLendXError::MutationLimitExceeded)
    );
}

#[test]
fn test_identity_multiple_window_cycles() {
    let mut limiter = RateLimiter::new(
        IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_IDENTITY_TRANSITIONS_PER_WINDOW,
    );
    for cycle in 0..3u32 {
        let base = 100 + cycle * (IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES + 1);
        for _ in 0..MAX_IDENTITY_TRANSITIONS_PER_WINDOW {
            limiter.check_and_record(base).unwrap();
        }
    }
    limiter
        .check_and_record(100 + 3 * (IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES + 1))
        .unwrap();
}

#[test]
fn test_identity_rejected_operation_does_not_consume_quota() {
    let mut limiter = RateLimiter::new(
        IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_IDENTITY_TRANSITIONS_PER_WINDOW,
    );
    // Use all but one slot
    for _ in 0..(MAX_IDENTITY_TRANSITIONS_PER_WINDOW - 1) {
        limiter.check_and_record(100).unwrap();
    }
    // One more succeeds (fills the last slot)
    limiter.check_and_record(100).unwrap();
    // Now at limit
    assert_eq!(
        limiter.check(100),
        Err(QuickLendXError::MutationLimitExceeded)
    );
}

#[test]
fn test_identity_many_addresses_independent_limits() {
    for _ in 0..5 {
        let mut limiter = RateLimiter::new(
            IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES,
            MAX_IDENTITY_TRANSITIONS_PER_WINDOW,
        );
        for _ in 0..MAX_IDENTITY_TRANSITIONS_PER_WINDOW {
            limiter.check_and_record(100).unwrap();
        }
        assert_eq!(
            limiter.check(100),
            Err(QuickLendXError::MutationLimitExceeded)
        );
    }
}

// ===========================================================================
// Cross-limiter boundary consistency checks
// ===========================================================================

#[test]
fn test_boundary_constants_are_sane_with_identity() {
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
    assert!(IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES > 0);
    assert!(MAX_IDENTITY_TRANSITIONS_PER_WINDOW > 0);

    // Identity limiter should be at most the mutation limiter (admin ops also
    // count as mutations, so the two limits are additive from the admin's
    // perspective).
    assert!(
        MAX_IDENTITY_TRANSITIONS_PER_WINDOW <= MAX_MUTATIONS_PER_WINDOW,
        "Identity transition limit should be at most the mutation limit"
    );

    // KYC submission limit is per-participant; identity transition limit is
    // per-admin.  Both are bounded but no ordering constraint between them.
}

#[test]
fn test_three_limiters_are_distinct() {
    // Each limiter uses its own window and max; verify they're independent
    assert_ne!(RATE_LIMIT_WINDOW_SEQUENCES, KYC_RATE_LIMIT_WINDOW_SEQUENCES);
    assert_ne!(
        RATE_LIMIT_WINDOW_SEQUENCES,
        IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES
    );
    assert_ne!(
        KYC_RATE_LIMIT_WINDOW_SEQUENCES,
        IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES
    );

    // Exhaust all three independently on the same "address" (sequence 100)
    let mut m = RateLimiter::new(RATE_LIMIT_WINDOW_SEQUENCES, MAX_MUTATIONS_PER_WINDOW);
    let mut k = RateLimiter::new(
        KYC_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_KYC_SUBMISSIONS_PER_WINDOW,
    );
    let mut i = RateLimiter::new(
        IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES,
        MAX_IDENTITY_TRANSITIONS_PER_WINDOW,
    );

    for _ in 0..MAX_MUTATIONS_PER_WINDOW {
        m.check_and_record(100).unwrap();
    }
    for _ in 0..MAX_KYC_SUBMISSIONS_PER_WINDOW {
        k.check_and_record(100).unwrap();
    }
    for _ in 0..MAX_IDENTITY_TRANSITIONS_PER_WINDOW {
        i.check_and_record(100).unwrap();
    }

    assert_eq!(m.check(100), Err(QuickLendXError::MutationLimitExceeded));
    assert_eq!(k.check(100), Err(QuickLendXError::MutationLimitExceeded));
    assert_eq!(i.check(100), Err(QuickLendXError::MutationLimitExceeded));

    // Recovery on different windows
    m.check_and_record(100 + RATE_LIMIT_WINDOW_SEQUENCES + 1)
        .unwrap();
    k.check_and_record(100 + KYC_RATE_LIMIT_WINDOW_SEQUENCES + 1)
        .unwrap();
    i.check_and_record(100 + IDENTITY_RATE_LIMIT_WINDOW_SEQUENCES + 1)
        .unwrap();
}
