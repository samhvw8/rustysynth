#![allow(dead_code)]

use crate::synthesizer_settings::SynthesizerSettings;

#[derive(Debug)]
#[non_exhaustive]
pub(crate) struct Lfo {
    sample_rate: i32,
    block_size: usize,

    active: bool,

    delay: f64,
    period: f64,

    processed_sample_count: usize,
    value: f32,
}

impl Lfo {
    pub(crate) fn new(settings: &SynthesizerSettings) -> Self {
        Self {
            sample_rate: settings.sample_rate,
            block_size: settings.block_size,
            active: false,
            delay: 0_f64,
            period: 0_f64,
            processed_sample_count: 0,
            value: 0_f32,
        }
    }

    pub(crate) fn start(&mut self, delay: f32, frequency: f32) {
        if frequency > 1.0E-3_f32 {
            self.active = true;

            self.delay = delay as f64;
            self.period = 1.0_f64 / frequency as f64;

            self.processed_sample_count = 0;
            self.value = 0_f32;
        } else {
            self.active = false;
            self.value = 0_f32;
        }
    }

    pub(crate) fn process(&mut self) {
        if !self.active {
            return;
        }

        self.processed_sample_count += self.block_size;

        let current_time = self.processed_sample_count as f64 / self.sample_rate as f64;

        if current_time < self.delay {
            self.value = 0_f32;
        } else {
            let phase = fmod(current_time - self.delay, self.period) / self.period;
            if phase < 0.25 {
                self.value = (4_f64 * phase) as f32;
            } else if phase < 0.75 {
                self.value = (4_f64 * (0.5 - phase)) as f32;
            } else {
                self.value = (4_f64 * (phase - 1.0)) as f32;
            }
        }
    }

    pub(crate) fn get_value(&self) -> f32 {
        self.value
    }
}

// `%` on f64 compiles to Rust's own software fmod on Linux (compiler_builtins, a bit-by-bit loop
// that took ~4% of render time on x86), and linking libm's instead is not reliable. fmod's result
// is always exactly representable, so one fused multiply-add with the right integer quotient
// reproduces it bit for bit; the quotient from the division can be off by one, which the range
// check corrects. Only called with x >= 0 and y > 0.
fn fmod(x: f64, y: f64) -> f64 {
    if x < y {
        return x;
    }
    let mut n = (x / y).trunc();
    let mut r = (-n).mul_add(y, x);
    if r < 0.0 || (r == 0.0 && r.is_sign_negative()) {
        n -= 1.0;
        r = (-n).mul_add(y, x);
    } else if r >= y {
        n += 1.0;
        r = (-n).mul_add(y, x);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::fmod;
    use crate::test_util::Rng;

    #[test]
    fn fmod_matches_the_exact_remainder() {
        let mut rng = Rng::new(7);
        for i in 0..2_000_000u64 {
            let y = match i % 3 {
                0 => rng.range(0.01, 2.0) as f64,
                1 => 1.0 / rng.range(0.1, 20.0) as f64,
                _ => 1.0 / ((i % 97) as f64 + 0.5),
            };
            let x = match i % 4 {
                0 => rng.range(0.0, 600.0) as f64,
                1 => (i / 4) as f64 * 64.0 / 44_100.0 - rng.range(0.0, 2.0) as f64,
                2 => y * (rng.below(10_000) as f64),
                _ => y * (rng.below(10_000) as f64) + f64::EPSILON * (rng.below(9) as f64 - 4.0),
            };
            if x < 0.0 {
                continue;
            }
            let want = x % y;
            let got = fmod(x, y);
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "fmod({x:e}, {y:e}): {got:e} vs {want:e}"
            );
        }
    }
}
