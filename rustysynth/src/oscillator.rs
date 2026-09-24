#![allow(dead_code)]

use crate::loop_mode::LoopMode;
use crate::synthesizer_settings::SynthesizerSettings;

// In this class, fixed-point numbers are used for speed-up.
// A fixed-point number is expressed by Int64, whose lower 24 bits represent the fraction part,
// and the rest represent the integer part.
// For clarity, fixed-point number variables have a suffix "_fp".

#[derive(Debug)]
#[non_exhaustive]
pub(crate) struct Oscillator {
    synthesizer_sample_rate: i32,

    loop_mode: LoopMode,
    sample_sample_rate: i32,
    start: i32,
    end: i32,
    start_loop: i32,
    end_loop: i32,
    root_key: i32,

    tune: f32,
    pitch_change_scale: f32,
    sample_rate_ratio: f32,

    looping: bool,

    position_fp: i64,

    // Pitch is unchanged from the previous block for ~98% of voice blocks; reuse the ratio
    // instead of calling powf again. Same input, same result.
    last_pitch: f32,
    last_pitch_ratio: f32,
}

impl Oscillator {
    const FRAC_BITS: i32 = 24;
    const FRAC_UNIT: i64 = 1_i64 << Oscillator::FRAC_BITS;
    const FP_TO_SAMPLE: f32 = 1_f32 / (32768 * Oscillator::FRAC_UNIT) as f32;

    pub(crate) fn new(settings: &SynthesizerSettings) -> Self {
        Self {
            synthesizer_sample_rate: settings.sample_rate,
            loop_mode: LoopMode::NoLoop,
            sample_sample_rate: 0,
            start: 0,
            end: 0,
            start_loop: 0,
            end_loop: 0,
            root_key: 0,
            tune: 0_f32,
            pitch_change_scale: 0_f32,
            sample_rate_ratio: 0_f32,
            looping: false,
            position_fp: 0,
            last_pitch: f32::NAN,
            last_pitch_ratio: 0_f32,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        &mut self,
        loop_mode: LoopMode,
        sample_rate: i32,
        start: i32,
        end: i32,
        start_loop: i32,
        end_loop: i32,
        root_key: i32,
        coarse_tune: i32,
        fine_tune: i32,
        scale_tuning: i32,
    ) {
        self.loop_mode = loop_mode;
        self.sample_sample_rate = sample_rate;
        self.start = start;
        self.end = end;
        self.start_loop = start_loop;
        self.end_loop = end_loop;
        self.root_key = root_key;

        self.tune = coarse_tune as f32 + 0.01_f32 * fine_tune as f32;
        self.pitch_change_scale = 0.01_f32 * scale_tuning as f32;
        self.sample_rate_ratio = sample_rate as f32 / self.synthesizer_sample_rate as f32;
        self.looping = self.loop_mode != LoopMode::NoLoop;
        self.position_fp = (start as i64) << Oscillator::FRAC_BITS;
        self.last_pitch = f32::NAN;
    }

    pub(crate) fn release(&mut self) {
        if self.loop_mode == LoopMode::LoopUntilNoteOff {
            self.looping = false;
        }
    }

    pub(crate) fn process(&mut self, data: &[i16], block: &mut [f32], pitch: f32) -> bool {
        if pitch.to_bits() != self.last_pitch.to_bits() {
            let pitch_change = self.pitch_change_scale * (pitch - self.root_key as f32) + self.tune;
            self.last_pitch_ratio = self.sample_rate_ratio * 2_f32.powf(pitch_change / 12_f32);
            self.last_pitch = pitch;
        }
        self.fill_block(data, block, self.last_pitch_ratio as f64)
    }

    fn fill_block(&mut self, data: &[i16], block: &mut [f32], pitch_ratio: f64) -> bool {
        let pitch_ratio_fp = (Oscillator::FRAC_UNIT as f64 * pitch_ratio) as i64;

        if self.looping {
            self.fill_block_continuous(data, block, pitch_ratio_fp)
        } else {
            self.fill_block_no_loop(data, block, pitch_ratio_fp)
        }
    }

    fn fill_block_no_loop(&mut self, data: &[i16], block: &mut [f32], pitch_ratio_fp: i64) -> bool {
        let last_fp = self.position_fp + (block.len() as i64 - 1) * pitch_ratio_fp;
        let last_index = (last_fp >> Oscillator::FRAC_BITS) as usize;
        if pitch_ratio_fp >= 0 && last_index < self.end as usize && last_index + 1 < data.len() {
            self.position_fp =
                Oscillator::interpolate(data, block, self.position_fp, pitch_ratio_fp);
            return true;
        }

        for t in 0..block.len() {
            let index = (self.position_fp >> Oscillator::FRAC_BITS) as usize;
            if index >= self.end as usize {
                if t > 0 {
                    let len = block.len();
                    block[t..len].fill(0_f32);
                    return true;
                } else {
                    return false;
                }
            }

            let x1 = data[index] as i64;
            let x2 = data[index + 1] as i64;
            let a_fp = self.position_fp & (Oscillator::FRAC_UNIT - 1);
            block[t] = Oscillator::FP_TO_SAMPLE
                * ((x1 << Oscillator::FRAC_BITS) + a_fp * (x2 - x1)) as f32;

            self.position_fp += pitch_ratio_fp;
        }

        true
    }

    fn fill_block_continuous(
        &mut self,
        data: &[i16],
        block: &mut [f32],
        pitch_ratio_fp: i64,
    ) -> bool {
        let end_loop_fp = (self.end_loop as i64) << Oscillator::FRAC_BITS;
        let loop_length = (self.end_loop - self.start_loop) as i64;
        let loop_length_fp = loop_length << Oscillator::FRAC_BITS;

        // Most blocks never reach the loop end: then no wrap-around is needed and the plain
        // interpolation loop gives the same samples.
        let last_fp = self.position_fp + (block.len() as i64 - 1) * pitch_ratio_fp;
        if pitch_ratio_fp >= 0
            && self.position_fp < end_loop_fp
            && (last_fp >> Oscillator::FRAC_BITS) + 1 < self.end_loop as i64
            && ((last_fp >> Oscillator::FRAC_BITS) as usize) + 1 < data.len()
        {
            self.position_fp =
                Oscillator::interpolate(data, block, self.position_fp, pitch_ratio_fp);
            return true;
        }

        for sample in block.iter_mut() {
            if self.position_fp >= end_loop_fp {
                self.position_fp -= loop_length_fp;
            }

            let index1 = (self.position_fp >> Oscillator::FRAC_BITS) as usize;
            let mut index2 = index1 + 1;
            if index2 >= self.end_loop as usize {
                index2 -= loop_length as usize;
            }

            let x1 = data[index1] as i64;
            let x2 = data[index2] as i64;
            let a_fp = self.position_fp & (Oscillator::FRAC_UNIT - 1);
            *sample = Oscillator::FP_TO_SAMPLE
                * ((x1 << Oscillator::FRAC_BITS) + a_fp * (x2 - x1)) as f32;

            self.position_fp += pitch_ratio_fp;
        }

        true
    }

    /// Linear interpolation for a stretch known to stay inside `data` and away from loop points.
    /// Returns the position after the block.
    fn interpolate(data: &[i16], block: &mut [f32], position_fp: i64, pitch_ratio_fp: i64) -> i64 {
        // A note at exactly the sample's pitch reads whole samples: the fraction stays zero, so each
        // output is x1 * 2^24 * FP_TO_SAMPLE = x1 / 32768 exactly (power-of-two scaling). A plain,
        // vectorisable copy gives the same bits. About 17% of voice blocks take this path.
        if pitch_ratio_fp == Oscillator::FRAC_UNIT && position_fp & (Oscillator::FRAC_UNIT - 1) == 0
        {
            let start = (position_fp >> Oscillator::FRAC_BITS) as usize;
            let len = block.len();
            for (sample, &x) in block.iter_mut().zip(&data[start..start + len]) {
                *sample = x as f32 * (1_f32 / 32768_f32);
            }
            return position_fp + block.len() as i64 * pitch_ratio_fp;
        }
        for (t, sample) in block.iter_mut().enumerate() {
            let position_fp = position_fp + t as i64 * pitch_ratio_fp;
            let index = (position_fp >> Oscillator::FRAC_BITS) as usize;
            let x1 = data[index] as i64;
            let x2 = data[index + 1] as i64;
            let a_fp = position_fp & (Oscillator::FRAC_UNIT - 1);
            *sample = Oscillator::FP_TO_SAMPLE
                * ((x1 << Oscillator::FRAC_BITS) + a_fp * (x2 - x1)) as f32;
        }
        position_fp + block.len() as i64 * pitch_ratio_fp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{bits, Rng};

    /// The per-sample loops as they were before the fast path, kept as the reference.
    fn reference_fill(
        osc: &mut Oscillator,
        data: &[i16],
        block: &mut [f32],
        pitch_ratio_fp: i64,
    ) -> bool {
        if !osc.looping {
            for t in 0..block.len() {
                let index = (osc.position_fp >> Oscillator::FRAC_BITS) as usize;
                if index >= osc.end as usize {
                    if t > 0 {
                        block[t..].fill(0_f32);
                        return true;
                    }
                    return false;
                }
                let x1 = data[index] as i64;
                let x2 = data[index + 1] as i64;
                let a_fp = osc.position_fp & (Oscillator::FRAC_UNIT - 1);
                block[t] = Oscillator::FP_TO_SAMPLE
                    * ((x1 << Oscillator::FRAC_BITS) + a_fp * (x2 - x1)) as f32;
                osc.position_fp += pitch_ratio_fp;
            }
            return true;
        }
        let end_loop_fp = (osc.end_loop as i64) << Oscillator::FRAC_BITS;
        let loop_length = (osc.end_loop - osc.start_loop) as i64;
        let loop_length_fp = loop_length << Oscillator::FRAC_BITS;
        for sample in block.iter_mut() {
            if osc.position_fp >= end_loop_fp {
                osc.position_fp -= loop_length_fp;
            }
            let index1 = (osc.position_fp >> Oscillator::FRAC_BITS) as usize;
            let mut index2 = index1 + 1;
            if index2 >= osc.end_loop as usize {
                index2 -= loop_length as usize;
            }
            let x1 = data[index1] as i64;
            let x2 = data[index2] as i64;
            let a_fp = osc.position_fp & (Oscillator::FRAC_UNIT - 1);
            *sample = Oscillator::FP_TO_SAMPLE
                * ((x1 << Oscillator::FRAC_BITS) + a_fp * (x2 - x1)) as f32;
            osc.position_fp += pitch_ratio_fp;
        }
        true
    }

    struct Case {
        /// SF2 `sampleModes`: 0 no loop, 1 continuous, 3 loop until note-off.
        mode: i16,
        start: i32,
        end: i32,
        start_loop: i32,
        end_loop: i32,
    }

    fn oscillator(case: &Case) -> Oscillator {
        let mut osc = Oscillator::new(&SynthesizerSettings::new(44_100));
        osc.start(
            LoopMode::from_i16(case.mode),
            44_100,
            case.start,
            case.end,
            case.start_loop,
            case.end_loop,
            60,
            0,
            0,
            100,
        );
        osc
    }

    fn assert_matches_reference(case: Case, seed: u64) {
        let mut rng = Rng::new(seed);
        // Real sample data is followed by at least 46 zero samples in a SoundFont.
        let data: Vec<i16> = (0..case.end as usize + 46)
            .map(|i| {
                if i < case.end as usize {
                    (rng.next_u64() >> 48) as i16
                } else {
                    0
                }
            })
            .collect();
        // Pitch ratios from four octaves down to four octaves up, plus unison.
        for ratio in [1.0_f64, 0.0625, 0.5, 0.999, 1.5, 2.0, 3.7, 16.0] {
            let mut fast = oscillator(&case);
            let mut reference = oscillator(&case);
            let ratio_fp = (Oscillator::FRAC_UNIT as f64 * ratio) as i64;
            for n in 0..400 {
                if n == 200 {
                    fast.release();
                    reference.release();
                }
                let mut a = vec![9_f32; 64];
                let mut b = vec![9_f32; 64];
                let alive_a = fast.fill_block(&data, &mut a, ratio);
                let alive_b = reference_fill(&mut reference, &data, &mut b, ratio_fp);
                assert_eq!(alive_a, alive_b, "ratio {ratio}, block {n}");
                if !alive_a {
                    break;
                }
                assert_eq!(bits(&a), bits(&b), "ratio {ratio}, block {n}");
                assert_eq!(
                    fast.position_fp, reference.position_fp,
                    "ratio {ratio}, block {n}"
                );
            }
        }
    }

    #[test]
    fn no_loop_matches_reference_including_the_sample_end() {
        assert_matches_reference(
            Case {
                mode: 0,
                start: 0,
                end: 5_000,
                start_loop: 0,
                end_loop: 0,
            },
            1,
        );
        assert_matches_reference(
            Case {
                mode: 0,
                start: 1_000,
                end: 1_100,
                start_loop: 0,
                end_loop: 0,
            },
            2,
        );
    }

    #[test]
    fn no_loop_block_ending_exactly_on_the_sample_end_matches_reference() {
        // At unison the second block covers indices 64..=127: its last sample sits exactly on `end`,
        // which must already be silent.
        for end in [126, 127, 128] {
            assert_matches_reference(
                Case {
                    mode: 0,
                    start: 0,
                    end,
                    start_loop: 0,
                    end_loop: 0,
                },
                end as u64,
            );
        }
    }

    #[test]
    fn continuous_loop_matches_reference_across_wraps() {
        assert_matches_reference(
            Case {
                mode: 1,
                start: 0,
                end: 4_000,
                start_loop: 1_000,
                end_loop: 3_000,
            },
            3,
        );
        // Loops shorter than one block wrap several times per block.
        assert_matches_reference(
            Case {
                mode: 1,
                start: 0,
                end: 600,
                start_loop: 500,
                end_loop: 520,
            },
            4,
        );
    }

    #[test]
    fn loop_until_note_off_matches_reference_through_release() {
        assert_matches_reference(
            Case {
                mode: 3,
                start: 0,
                end: 3_000,
                start_loop: 100,
                end_loop: 900,
            },
            5,
        );
    }
}
