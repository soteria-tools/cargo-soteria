//! Fixture crate for cargo-soteria's parallel test runner.
//!
//! ~30 `#[soteria::test]` entry points: a deliberate mix of tests that should
//! pass (true properties), tests that should fail (false properties and real
//! bugs such as arithmetic overflow and out-of-bounds indexing), and one slow
//! test that explores many symbolic branches.

#[cfg(soteria)]
mod verification {
    // ── should PASS: true properties ────────────────────────────────────────

    #[soteria::test]
    fn not_not_bool_is_identity() {
        let b: bool = soteria::nondet_bytes();
        assert!(!!b == b, "!!b == b");
    }

    #[soteria::test]
    fn not_not_u8_is_identity() {
        let x: u8 = soteria::nondet_bytes();
        assert!(!!x == x, "bitwise !!x == x");
    }

    #[soteria::test]
    fn xor_self_is_zero() {
        let x: u32 = soteria::nondet_bytes();
        assert!(x ^ x == 0, "x ^ x == 0");
    }

    #[soteria::test]
    fn and_self_is_idempotent() {
        let x: u32 = soteria::nondet_bytes();
        assert!(x & x == x, "x & x == x");
    }

    #[soteria::test]
    fn or_self_is_idempotent() {
        let x: u32 = soteria::nondet_bytes();
        assert!(x | x == x, "x | x == x");
    }

    #[soteria::test]
    fn wrapping_add_is_commutative() {
        let a: u32 = soteria::nondet_bytes();
        let b: u32 = soteria::nondet_bytes();
        assert!(a.wrapping_add(b) == b.wrapping_add(a), "a + b == b + a");
    }

    #[soteria::test]
    fn wrapping_mul_is_commutative() {
        let a: u32 = soteria::nondet_bytes();
        let b: u32 = soteria::nondet_bytes();
        assert!(a.wrapping_mul(b) == b.wrapping_mul(a), "a * b == b * a");
    }

    #[soteria::test]
    fn add_zero_is_identity() {
        let a: u32 = soteria::nondet_bytes();
        assert!(a.wrapping_add(0) == a, "a + 0 == a");
    }

    #[soteria::test]
    fn or_is_at_least_operand() {
        let a: u8 = soteria::nondet_bytes();
        let b: u8 = soteria::nondet_bytes();
        assert!(a | b >= a, "a | b >= a");
    }

    #[soteria::test]
    fn and_is_at_most_operand() {
        let a: u8 = soteria::nondet_bytes();
        let b: u8 = soteria::nondet_bytes();
        assert!(a & b <= a, "a & b <= a");
    }

    #[soteria::test]
    fn min_is_at_most_both() {
        let a: u32 = soteria::nondet_bytes();
        let b: u32 = soteria::nondet_bytes();
        let m = a.min(b);
        assert!(m <= a && m <= b, "min(a,b) <= a and <= b");
    }

    #[soteria::test]
    fn max_is_at_least_both() {
        let a: u32 = soteria::nondet_bytes();
        let b: u32 = soteria::nondet_bytes();
        let m = a.max(b);
        assert!(m >= a && m >= b, "max(a,b) >= a and >= b");
    }

    #[soteria::test]
    fn min_is_commutative() {
        let a: u32 = soteria::nondet_bytes();
        let b: u32 = soteria::nondet_bytes();
        assert!(a.min(b) == b.min(a), "min(a,b) == min(b,a)");
    }

    #[soteria::test]
    fn abs_diff_is_symmetric() {
        let a: u32 = soteria::nondet_bytes();
        let b: u32 = soteria::nondet_bytes();
        assert!(a.abs_diff(b) == b.abs_diff(a), "abs_diff symmetric");
    }

    #[soteria::test]
    fn abs_diff_self_is_zero() {
        let a: u32 = soteria::nondet_bytes();
        assert!(a.abs_diff(a) == 0, "abs_diff(a,a) == 0");
    }

    #[soteria::test]
    fn shift_left_one_is_double() {
        let a: u8 = soteria::nondet_bytes();
        assert!(a.wrapping_shl(1) == a.wrapping_mul(2), "a << 1 == a * 2");
    }

    #[soteria::test]
    fn saturating_add_is_at_least_operand() {
        let a: u8 = soteria::nondet_bytes();
        let b: u8 = soteria::nondet_bytes();
        assert!(a.saturating_add(b) >= a, "sat_add(a,b) >= a");
    }

    #[soteria::test]
    fn saturating_sub_is_at_most_operand() {
        let a: u8 = soteria::nondet_bytes();
        let b: u8 = soteria::nondet_bytes();
        assert!(a.saturating_sub(b) <= a, "sat_sub(a,b) <= a");
    }

    #[soteria::test]
    fn clamp_stays_in_range() {
        let x: i32 = soteria::nondet_bytes();
        let c = x.clamp(0, 100);
        assert!(c >= 0 && c <= 100, "clamp result in [0,100]");
    }

    #[soteria::test]
    fn count_ones_within_width() {
        let x: u8 = soteria::nondet_bytes();
        assert!(x.count_ones() <= 8, "u8 has at most 8 set bits");
    }

    #[soteria::test]
    fn assume_narrows_range() {
        let x: u32 = soteria::nondet_bytes();
        soteria::assume(x < 10);
        assert!(x < 100, "x < 10 implies x < 100");
    }

    // ── should FAIL: false properties ───────────────────────────────────────

    #[soteria::test]
    fn add_is_not_identity() {
        let a: u32 = soteria::nondet_bytes();
        let b: u32 = soteria::nondet_bytes();
        // False whenever b != 0.
        assert!(a.wrapping_add(b) == a, "WRONG: a + b == a");
    }

    #[soteria::test]
    fn successor_is_always_greater() {
        let a: u8 = soteria::nondet_bytes();
        // False at a == 255 (wraps to 0).
        assert!(a.wrapping_add(1) > a, "WRONG: a + 1 > a");
    }

    #[soteria::test]
    fn double_equals_self() {
        let a: u32 = soteria::nondet_bytes();
        // False whenever a != 0.
        assert!(a.wrapping_mul(2) == a, "WRONG: 2a == a");
    }

    #[soteria::test]
    fn min_is_always_first() {
        let a: u32 = soteria::nondet_bytes();
        let b: u32 = soteria::nondet_bytes();
        // False whenever b < a.
        assert!(a.min(b) == a, "WRONG: min(a,b) == a");
    }

    #[soteria::test]
    fn half_then_double_is_identity() {
        let a: u8 = soteria::nondet_bytes();
        // False for odd a (integer division truncates).
        assert!((a / 2).wrapping_mul(2) == a, "WRONG: (a/2)*2 == a");
    }

    #[soteria::test]
    fn xor_is_always_zero() {
        let a: u8 = soteria::nondet_bytes();
        let b: u8 = soteria::nondet_bytes();
        // False whenever a != b.
        assert!(a ^ b == 0, "WRONG: a ^ b == 0");
    }

    #[soteria::test]
    fn unchecked_add_can_overflow() {
        let a: u8 = soteria::nondet_bytes();
        let b: u8 = soteria::nondet_bytes();
        // Real bug: non-wrapping add overflows for large a + b.
        let _ = a + b;
    }

    #[soteria::test]
    fn index_can_go_out_of_bounds() {
        let arr = [10u8, 20, 30, 40];
        let i: usize = soteria::nondet_bytes();
        // Real bug: out-of-bounds read when i >= 4.
        assert!(arr[i] < 100, "in-bounds element is small");
    }

    // ── slow: many symbolic branches (widens the Ctrl-C window) ──────────────

    #[soteria::test]
    fn slow_symbolic_loop() {
        let n: u8 = soteria::nondet_bytes();
        soteria::assume(n <= 60);
        let mut i: u8 = 0;
        let mut acc: u32 = 0;
        while i < n {
            acc = acc.wrapping_add(i as u32);
            i = i.wrapping_add(1);
        }
        // Always true: the loop runs exactly until i reaches n.
        assert!(i == n, "loop terminates with i == n");
    }
}
