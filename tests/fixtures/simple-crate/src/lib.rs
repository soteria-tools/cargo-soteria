pub fn negate(b: bool) -> bool {
    !b
}

pub fn abs_diff(a: u32, b: u32) -> u32 {
    if a >= b { a - b } else { b - a }
}

#[cfg(soteria)]
mod verification {
    use super::*;

    #[soteria::test]
    fn double_negation_is_identity() {
        let b: bool = soteria::nondet_bytes();
        soteria::assert(negate(negate(b)) == b, "!!b == b");
    }

    #[soteria::test]
    fn abs_diff_is_symmetric() {
        let a: u32 = soteria::nondet_bytes();
        let b: u32 = soteria::nondet_bytes();
        soteria::assert(abs_diff(a, b) == abs_diff(b, a), "abs_diff(a,b) == abs_diff(b,a)");
    }
}
