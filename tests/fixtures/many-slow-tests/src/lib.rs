//! Fixture crate for cargo-soteria's parallel test runner — the "slow" variant.
//!
//! 20 `#[soteria::test]` entry points (`test_1`..`test_20`), each doing the same
//! work: a long concrete loop (`access`) that increments a counter a fixed
//! number of times, with bounds spread between 1000 and 2000. Each test is
//! individually slow to execute, which exercises the runner's parallelism and
//! widens the Ctrl-C window.

#[cfg(soteria)]
mod verification {
    /// Increment `*x` exactly `n` times. The long loop is what makes each test slow.
    fn access(x: &mut u32, n: u32) {
        for _ in 0..n {
            *x += 1;
        }
    }

    #[soteria::test]
    fn test_1() {
        let mut x = 0;
        access(&mut x, 1000);
        assert!(x == 1000, "ok");
    }

    #[soteria::test]
    fn test_2() {
        let mut x = 0;
        access(&mut x, 1050);
        assert!(x == 1050, "ok");
    }

    #[soteria::test]
    fn test_3() {
        let mut x = 0;
        access(&mut x, 1100);
        assert!(x == 1100, "ok");
    }

    #[soteria::test]
    fn test_4() {
        let mut x = 0;
        access(&mut x, 1150);
        assert!(x == 1150, "ok");
    }

    #[soteria::test]
    fn test_5() {
        let mut x = 0;
        access(&mut x, 1200);
        assert!(x == 1200, "ok");
    }

    #[soteria::test]
    fn test_6() {
        let mut x = 0;
        access(&mut x, 1250);
        assert!(x == 1250, "ok");
    }

    #[soteria::test]
    fn test_7() {
        let mut x = 0;
        access(&mut x, 1300);
        assert!(x == 1300, "ok");
    }

    #[soteria::test]
    fn test_8() {
        let mut x = 0;
        access(&mut x, 1350);
        assert!(x == 1350, "ok");
    }

    #[soteria::test]
    fn test_9() {
        let mut x = 0;
        access(&mut x, 1400);
        assert!(x == 1400, "ok");
    }

    #[soteria::test]
    fn test_10() {
        let mut x = 0;
        access(&mut x, 1450);
        assert!(x == 1450, "ok");
    }

    #[soteria::test]
    fn test_11() {
        let mut x = 0;
        access(&mut x, 1500);
        assert!(x == 1500, "ok");
    }

    #[soteria::test]
    fn test_12() {
        let mut x = 0;
        access(&mut x, 1550);
        assert!(x == 1550, "ok");
    }

    #[soteria::test]
    fn test_13() {
        let mut x = 0;
        access(&mut x, 1600);
        assert!(x == 1600, "ok");
    }

    #[soteria::test]
    fn test_14() {
        let mut x = 0;
        access(&mut x, 1650);
        assert!(x == 1650, "ok");
    }

    #[soteria::test]
    fn test_15() {
        let mut x = 0;
        access(&mut x, 1700);
        assert!(x == 1700, "ok");
    }

    #[soteria::test]
    fn test_16() {
        let mut x = 0;
        access(&mut x, 1750);
        assert!(x == 1750, "ok");
    }

    #[soteria::test]
    fn test_17() {
        let mut x = 0;
        access(&mut x, 1800);
        assert!(x == 1800, "ok");
    }

    #[soteria::test]
    fn test_18() {
        let mut x = 0;
        access(&mut x, 1850);
        assert!(x == 1850, "ok");
    }

    #[soteria::test]
    fn test_19() {
        let mut x = 0;
        access(&mut x, 1900);
        assert!(x == 1900, "ok");
    }

    #[soteria::test]
    fn test_20() {
        let mut x = 0;
        access(&mut x, 1950);
        assert!(x == 1950, "ok");
    }
}
