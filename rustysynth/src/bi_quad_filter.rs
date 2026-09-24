#![allow(dead_code)]

use std::f32::consts;

use crate::synthesizer_settings::SynthesizerSettings;

#[derive(Debug)]
#[non_exhaustive]
pub(crate) struct BiQuadFilter {
    sample_rate: i32,

    active: bool,

    a0: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    a4: f32,

    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,

    // Inputs of the last coefficient calculation. Modulated voices call set_low_pass_filter every
    // block, but once their envelopes settle the cutoff stops changing and cos/sin can be skipped.
    last_cutoff: f32,
    last_resonance: f32,
}

impl BiQuadFilter {
    const RESONANCE_PEAK_OFFSET: f32 = 1_f32 - 1_f32 / core::f32::consts::SQRT_2;

    pub(crate) fn new(settings: &SynthesizerSettings) -> Self {
        Self {
            sample_rate: settings.sample_rate,
            active: false,
            a0: 0_f32,
            a1: 0_f32,
            a2: 0_f32,
            a3: 0_f32,
            a4: 0_f32,
            x1: 0_f32,
            x2: 0_f32,
            y1: 0_f32,
            y2: 0_f32,
            last_cutoff: f32::NAN,
            last_resonance: f32::NAN,
        }
    }

    pub(crate) fn clear_buffer(&mut self) {
        self.x1 = 0_f32;
        self.x2 = 0_f32;
        self.y1 = 0_f32;
        self.y2 = 0_f32;
    }

    pub(crate) fn set_low_pass_filter(&mut self, cutoff_frequency: f32, resonance: f32) {
        if cutoff_frequency == self.last_cutoff && resonance == self.last_resonance {
            return;
        }
        self.last_cutoff = cutoff_frequency;
        self.last_resonance = resonance;
        if cutoff_frequency < 0.499_f32 * self.sample_rate as f32 {
            self.active = true;

            // This equation gives the Q value which makes the desired resonance peak.
            // The error of the resultant peak height is less than 3%.
            let q = resonance
                - BiQuadFilter::RESONANCE_PEAK_OFFSET / (1_f32 + 6_f32 * (resonance - 1_f32));

            let w = 2_f32 * consts::PI * cutoff_frequency / self.sample_rate as f32;
            let cosw = w.cos();
            let alpha = w.sin() / (2_f32 * q);

            let b0 = (1_f32 - cosw) / 2_f32;
            let b1 = 1_f32 - cosw;
            let b2 = (1_f32 - cosw) / 2_f32;
            let a0 = 1_f32 + alpha;
            let a1 = -2_f32 * cosw;
            let a2 = 1_f32 - alpha;

            self.set_coefficients(a0, a1, a2, b0, b1, b2);
        } else {
            self.active = false;
        }
    }

    pub(crate) fn process(&mut self, block: &mut [f32]) {
        let block_length = block.len();

        if self.active {
            for input in block.iter_mut().take(block_length) {
                let output = self.a0 * *input + self.a1 * self.x1 + self.a2 * self.x2
                    - self.a3 * self.y1
                    - self.a4 * self.y2;

                self.x2 = self.x1;
                self.x1 = *input;
                self.y2 = self.y1;
                self.y1 = output;

                *input = output;
            }
        } else {
            self.x2 = block[block_length - 2];
            self.x1 = block[block_length - 1];
            self.y2 = self.x2;
            self.y1 = self.x1;
        }
    }

    /// Filters the blocks of every active voice, four voices at a time.
    ///
    /// A single filter is a serial recurrence, so one voice at a time leaves the CPU waiting on it.
    /// Four independent voices in lockstep let the CPU overlap their dependency chains (the code
    /// stays scalar; the gain is instruction-level parallelism, not SIMD). Each lane evaluates
    /// the same expression in the same order as `process`, so the output is bit-identical.
    pub(crate) fn process_voices<'a>(
        voices: impl Iterator<Item = (&'a mut BiQuadFilter, &'a mut [f32])>,
    ) {
        let mut active = voices.filter_map(|(filter, block)| {
            if filter.active {
                Some((filter, block))
            } else {
                filter.process(block);
                None
            }
        });
        loop {
            match (active.next(), active.next(), active.next(), active.next()) {
                (Some(a), Some(b), Some(c), Some(d)) => BiQuadFilter::process4([a, b, c, d]),
                (a, b, c, _) => {
                    for (filter, block) in [a, b, c].into_iter().flatten() {
                        filter.process(block);
                    }
                    return;
                }
            }
        }
    }

    #[allow(clippy::needless_range_loop)]
    fn process4(lanes: [(&mut BiQuadFilter, &mut [f32]); 4]) {
        let [(f0, b0), (f1, b1), (f2, b2), (f3, b3)] = lanes;
        let filters = [f0, f1, f2, f3];
        let blocks = [b0, b1, b2, b3];
        let length = blocks.iter().map(|b| b.len()).min().unwrap();

        let a0: [f32; 4] = std::array::from_fn(|k| filters[k].a0);
        let a1: [f32; 4] = std::array::from_fn(|k| filters[k].a1);
        let a2: [f32; 4] = std::array::from_fn(|k| filters[k].a2);
        let a3: [f32; 4] = std::array::from_fn(|k| filters[k].a3);
        let a4: [f32; 4] = std::array::from_fn(|k| filters[k].a4);
        let mut x1: [f32; 4] = std::array::from_fn(|k| filters[k].x1);
        let mut x2: [f32; 4] = std::array::from_fn(|k| filters[k].x2);
        let mut y1: [f32; 4] = std::array::from_fn(|k| filters[k].y1);
        let mut y2: [f32; 4] = std::array::from_fn(|k| filters[k].y2);

        for t in 0..length {
            let input: [f32; 4] = std::array::from_fn(|k| blocks[k][t]);
            let output: [f32; 4] = std::array::from_fn(|k| {
                a0[k] * input[k] + a1[k] * x1[k] + a2[k] * x2[k] - a3[k] * y1[k] - a4[k] * y2[k]
            });
            x2 = x1;
            x1 = input;
            y2 = y1;
            y1 = output;
            for k in 0..4 {
                blocks[k][t] = output[k];
            }
        }

        for (k, filter) in filters.into_iter().enumerate() {
            filter.x1 = x1[k];
            filter.x2 = x2[k];
            filter.y1 = y1[k];
            filter.y2 = y2[k];
        }
    }

    fn set_coefficients(&mut self, a0: f32, a1: f32, a2: f32, b0: f32, b1: f32, b2: f32) {
        self.a0 = b0 / a0;
        self.a1 = b1 / a0;
        self.a2 = b2 / a0;
        self.a3 = a1 / a0;
        self.a4 = a2 / a0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{bits, Rng};

    const RATE: i32 = 44_100;

    fn filters(n: usize, rng: &mut Rng) -> Vec<BiQuadFilter> {
        let settings = SynthesizerSettings::new(RATE);
        (0..n)
            .map(|_| {
                let mut f = BiQuadFilter::new(&settings);
                // About a quarter above the Nyquist guard, which leaves the filter inactive.
                let cutoff = if rng.below(4) == 0 {
                    30_000.0
                } else {
                    rng.range(20.0, 20_000.0)
                };
                f.set_low_pass_filter(cutoff, rng.range(1.0, 10.0));
                f
            })
            .collect()
    }

    #[test]
    fn cached_coefficients_equal_recomputed_ones() {
        let settings = SynthesizerSettings::new(RATE);
        let mut rng = Rng::new(99);
        let mut cached = BiQuadFilter::new(&settings);
        for _ in 0..10_000 {
            // Repeat values often, as settled envelopes do, so the cache is exercised.
            let (cutoff, q) = if rng.below(3) == 0 {
                (cached.last_cutoff, cached.last_resonance)
            } else {
                (rng.range(20.0, 25_000.0), rng.range(1.0, 10.0))
            };
            if cutoff.is_nan() {
                continue;
            }
            cached.set_low_pass_filter(cutoff, q);
            let mut fresh = BiQuadFilter::new(&settings);
            fresh.set_low_pass_filter(cutoff, q);
            assert_eq!(cached.active, fresh.active);
            if !fresh.active {
                // An inactive filter never reads its coefficients.
                continue;
            }
            for (a, b) in [
                (cached.a0, fresh.a0),
                (cached.a1, fresh.a1),
                (cached.a2, fresh.a2),
                (cached.a3, fresh.a3),
                (cached.a4, fresh.a4),
            ] {
                assert_eq!(a.to_bits(), b.to_bits());
            }
        }
    }

    #[test]
    fn voices_in_lockstep_are_bit_identical_to_one_voice_at_a_time() {
        // 0..=20 covers empty, remainders below four, one and several groups of four.
        for voices in 0..=20 {
            let mut rng = Rng::new(voices as u64 + 10);
            let mut lockstep = filters(voices, &mut rng);
            let mut rng = Rng::new(voices as u64 + 10);
            let mut serial = filters(voices, &mut rng);

            for n in 0..100 {
                if n % 25 == 0 {
                    // Cutoff changes mid-note, as with a modulation envelope.
                    for (a, b) in lockstep.iter_mut().zip(serial.iter_mut()) {
                        let (cutoff, q) = (rng.range(20.0, 25_000.0), rng.range(1.0, 5.0));
                        a.set_low_pass_filter(cutoff, q);
                        b.set_low_pass_filter(cutoff, q);
                    }
                }
                let blocks: Vec<Vec<f32>> = (0..voices).map(|_| rng.block(64)).collect();
                let mut out_lockstep = blocks.clone();
                let mut out_serial = blocks;
                BiQuadFilter::process_voices(
                    lockstep
                        .iter_mut()
                        .zip(out_lockstep.iter_mut().map(|b| &mut b[..])),
                );
                for (f, b) in serial.iter_mut().zip(out_serial.iter_mut()) {
                    f.process(b);
                }
                for v in 0..voices {
                    assert_eq!(
                        bits(&out_lockstep[v]),
                        bits(&out_serial[v]),
                        "{voices} voices, block {n}, voice {v}"
                    );
                }
            }
        }
    }
}
