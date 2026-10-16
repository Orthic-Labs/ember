//! A small deterministic generator (SplitMix64 seeding, xoshiro256**), so every pair is reproducible from
//! `(seed, step, index)` regardless of thread scheduling. Not for cryptography.

#[derive(Clone, Debug)]
pub struct Rng {
    s: [u64; 4],
    spare: Option<f64>,
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Rng {
    pub fn new(seed: u64) -> Rng {
        let mut sm = seed;
        let mut s = [0u64; 4];
        for v in &mut s {
            *v = splitmix(&mut sm);
        }
        Rng { s, spare: None }
    }

    /// A stream named by several integers (seed, step, sample index, ...): order matters, neighbours are unrelated.
    pub fn from_parts(parts: &[u64]) -> Rng {
        let mut acc = 0x243F_6A88_85A3_08D3u64;
        for &p in parts {
            let mut st = acc ^ p.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            acc = splitmix(&mut st);
        }
        Rng::new(acc)
    }

    pub fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// Uniform in `[0, 1)` with 53 bits.
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.uniform()
    }

    /// Log-uniform in `[lo, hi]` (both positive).
    pub fn log_range(&mut self, lo: f64, hi: f64) -> f64 {
        (lo.ln() + (hi.ln() - lo.ln()) * self.uniform()).exp()
    }

    /// Uniform integer in `0..n` (`0` when `n == 0`).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() % n as u64) as usize }
    }

    /// Standard normal (Box-Muller, both values used).
    pub fn normal(&mut self) -> f64 {
        if let Some(v) = self.spare.take() {
            return v;
        }
        let u1 = loop {
            let u = self.uniform();
            if u > 1e-300 {
                break u;
            }
        };
        let u2 = self.uniform();
        let r = (-2.0 * u1.ln()).sqrt();
        let th = std::f64::consts::TAU * u2;
        self.spare = Some(r * th.sin());
        r * th.cos()
    }

    /// Poisson count with mean `lambda`: Knuth's product method below 50 (exact), the normal approximation above
    /// (mean and variance exact, skewness `1/sqrt(lambda)` < 0.15 there). Negative or non-finite means give 0.
    pub fn poisson(&mut self, lambda: f64) -> f64 {
        if !lambda.is_finite() || lambda <= 0.0 {
            return 0.0;
        }
        if lambda < 50.0 {
            let limit = (-lambda).exp();
            let mut k = 0.0;
            let mut p = self.uniform();
            while p > limit {
                k += 1.0;
                p *= self.uniform();
            }
            k
        } else {
            (lambda + lambda.sqrt() * self.normal()).round().max(0.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_parts_same_stream_different_parts_differ() {
        let mut a = Rng::from_parts(&[1, 2, 3]);
        let mut b = Rng::from_parts(&[1, 2, 3]);
        let mut c = Rng::from_parts(&[1, 2, 4]);
        let (x, y, z) = (a.next_u64(), b.next_u64(), c.next_u64());
        assert_eq!(x, y);
        assert_ne!(x, z);
    }

    #[test]
    fn normal_and_uniform_moments() {
        let mut r = Rng::new(7);
        let n = 200_000;
        let (mut s, mut s2, mut u) = (0.0, 0.0, 0.0);
        for _ in 0..n {
            let v = r.normal();
            s += v;
            s2 += v * v;
            u += r.uniform();
        }
        let mean = s / n as f64;
        let var = s2 / n as f64 - mean * mean;
        assert!(mean.abs() < 0.01, "mean {mean}");
        assert!((var - 1.0).abs() < 0.02, "var {var}");
        assert!((u / n as f64 - 0.5).abs() < 0.01);
    }
}
