pub fn negate(b: bool) -> bool {
    !b
}

pub fn abs_diff(a: u32, b: u32) -> u32 {
    if a >= b {
        a - b
    } else {
        b - a
    }
}

#[cfg(kani)]
mod verification {
    use super::*;

    #[kani::proof]
    fn double_negation_is_identity() {
        let b: bool = kani::any();
        assert!(negate(negate(b)) == b, "!!b == b");
    }

    #[kani::proof]
    fn abs_diff_is_symmetric() {
        let a: u32 = kani::any();
        let b: u32 = kani::any();
        assert!(
            abs_diff(a, b) == abs_diff(b, a),
            "abs_diff(a,b) == abs_diff(b,a)",
        );
    }
}
