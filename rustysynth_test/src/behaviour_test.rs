//! What audio-worker relies on: the rendered audio itself, not just parsing.

use crate::support::*;
use rustysynth::{LoopMode, SoundFont, SoundFontError};
use std::io::Cursor;
use std::sync::Arc;

/// Fingerprints of `ensemble_midi()` rendered for 7 s, recorded from the fork before the speed-up
/// patches (upstream ccfa1a7 + the loop-range repair). Any change here changes the audio.
const GOLDEN_TIMGM6MB: u64 = 0x60a0_b1df_9df1_8815;
const GOLDEN_MUSESCORE: u64 = 0x68db_c19a_5a4f_3c01;

fn check_golden(sound_font: &str, golden: u64) {
    let out = render(&load(sound_font), &ensemble_midi(), 7.0);
    assert_eq!(
        out.fingerprint(),
        golden,
        "{sound_font}: rendered audio changed (fingerprint {:#x})",
        out.fingerprint()
    );
}

#[test]
fn golden_render_timgm6mb() {
    check_golden("TimGM6mb.sf2", GOLDEN_TIMGM6MB);
}

#[test]
fn golden_render_generaluser_musescore() {
    check_golden("GeneralUser GS MuseScore v1.442.sf2", GOLDEN_MUSESCORE);
}

#[test]
fn render_is_deterministic() {
    let sound_font = load("TimGM6mb.sf2");
    let a = render(&sound_font, &ensemble_midi(), 7.0);
    let b = render(&sound_font, &ensemble_midi(), 7.0);
    assert_eq!(a.fingerprint(), b.fingerprint());
}

#[test]
fn concurrent_jobs_sharing_one_soundfont_render_the_same_audio() {
    // audio-worker keeps one SoundFont in an Arc and renders several jobs at once.
    let sound_font = load("GeneralUser GS MuseScore v1.442.sf2");
    let alone = render(&sound_font, &ensemble_midi(), 7.0).fingerprint();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let sound_font = Arc::clone(&sound_font);
            std::thread::spawn(move || render(&sound_font, &ensemble_midi(), 7.0).fingerprint())
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), alone);
    }
}

#[test]
fn ensemble_output_is_finite_audible_and_decays_to_silence() {
    for name in ["TimGM6mb.sf2", "GeneralUser GS MuseScore v1.442.sf2"] {
        let out = render(&load(name), &ensemble_midi(), 9.0);
        assert!(
            out.left.iter().chain(&out.right).all(|s| s.is_finite()),
            "{name}: NaN or infinity"
        );
        assert!(
            out.energy(0.5, 4.0) > 1e-4,
            "{name}: too quiet while playing"
        );
        // All notes end by 6 s; releases and reverb have died away 2.5 s later.
        assert!(
            out.energy(8.5, 9.0) < 1e-9,
            "{name}: still sounding at the end"
        );
    }
}

#[test]
fn empty_midi_renders_silence() {
    let out = render(&load("TimGM6mb.sf2"), &midi(Vec::new()), 1.0);
    assert_eq!(out.peak(), 0.0);
}

fn note_held_for_two_seconds() -> Vec<u8> {
    midi(vec![
        Event(0, [0x90, 69, 100]),
        Event(2 * TICKS_PER_SECOND, [0x80, 69, 0]),
    ])
}

fn synthetic(frames: u32, region: Region) -> Result<Arc<SoundFont>, SoundFontError> {
    SoundFont::new(&mut Cursor::new(synthetic_sf2(frames, &region))).map(Arc::new)
}

const HALF_SECOND: u32 = SAMPLE_RATE as u32 / 2;

#[test]
fn valid_loop_sustains_a_held_note() {
    let sf = synthetic(
        HALF_SECOND,
        Region {
            sample_modes: 1,
            start_loop: 1_000,
            end_loop: 21_000,
            end: None,
        },
    )
    .unwrap();
    assert_eq!(
        sf.get_instruments()[0].get_regions()[0].get_sample_modes(),
        LoopMode::Continuous
    );
    let out = render(&sf, &note_held_for_two_seconds(), 2.0);
    assert!(
        out.energy(1.0, 1.9) > out.energy(0.1, 0.4) * 0.1,
        "looped note should still sound after the sample ends"
    );
}

#[test]
fn loop_end_past_the_sample_data_plays_the_region_without_its_loop() {
    // Stock rustysynth rejects the whole SoundFont (sinshu/rustysynth#55), which is what happens with
    // Timbres of Heaven 3.4.
    let sf = synthetic(
        HALF_SECOND,
        Region {
            sample_modes: 1,
            start_loop: 1_000,
            end_loop: 90_000,
            end: None,
        },
    )
    .unwrap();
    assert_eq!(
        sf.get_instruments()[0].get_regions()[0].get_sample_modes(),
        LoopMode::NoLoop
    );
    let out = render(&sf, &note_held_for_two_seconds(), 2.0);
    let playing = out.energy(0.1, 0.4);
    assert!(playing > 1e-4, "the region still plays");
    // Only the reverb tail is left once the half-second sample has ended.
    assert!(
        out.energy(1.0, 1.9) < playing * 1e-3,
        "without a loop the note stops when the sample ends"
    );
}

#[test]
fn empty_or_inverted_loop_is_disabled_instead_of_rejected() {
    for (start_loop, end_loop) in [(5_000, 5_000), (9_000, 2_000)] {
        let sf = synthetic(
            HALF_SECOND,
            Region {
                sample_modes: 3,
                start_loop,
                end_loop,
                end: None,
            },
        )
        .unwrap();
        assert_eq!(
            sf.get_instruments()[0].get_regions()[0].get_sample_modes(),
            LoopMode::NoLoop
        );
    }
}

#[test]
fn garbage_loop_points_are_ignored_when_the_region_does_not_loop() {
    let sf = synthetic(
        HALF_SECOND,
        Region {
            sample_modes: 0,
            start_loop: 80_000,
            end_loop: 90_000,
            end: None,
        },
    );
    assert!(sf.is_ok());
}

#[test]
fn playback_range_past_the_sample_data_is_still_rejected() {
    let sf = synthetic(
        HALF_SECOND,
        Region {
            sample_modes: 0,
            start_loop: 0,
            end_loop: 0,
            end: Some(HALF_SECOND + 1_000),
        },
    );
    assert!(matches!(sf, Err(SoundFontError::SanityCheckFailed)));
}

/// Production's soundfont. Not redistributable here; set RUSTYSYNTH_TOH to its path to run.
#[test]
fn timbres_of_heaven_loads_and_every_preset_renders() {
    let Ok(path) = std::env::var("RUSTYSYNTH_TOH") else {
        eprintln!("skipped: set RUSTYSYNTH_TOH=/path/to/TOH.sf2");
        return;
    };
    let sf = Arc::new(SoundFont::new(&mut std::fs::File::open(path).unwrap()).unwrap());
    // Regions stock rustysynth rejects (it fails the whole load on any of them): all must now play
    // without a loop.
    let wave_length = sf.get_wave_data().len() as i32;
    let unusable: Vec<_> = sf
        .get_instruments()
        .iter()
        .flat_map(|i| i.get_regions())
        .filter(|r| {
            let (start, end) = (r.get_sample_start_loop(), r.get_sample_end_loop());
            start < 0 || end >= wave_length || end < start
        })
        .collect();
    eprintln!("regions with unusable loops: {}", unusable.len());
    assert!(
        !unusable.is_empty(),
        "Timbres of Heaven 3.4 has regions with unusable loops"
    );
    assert!(unusable
        .iter()
        .all(|r| r.get_sample_modes() == LoopMode::NoLoop));
    for preset in sf.get_presets() {
        let (bank, program) = (preset.get_bank_number(), preset.get_patch_number());
        let ch = if bank == 128 { 9 } else { 0 };
        let events = vec![
            Event(0, [0xB0 | ch, 0, (bank.min(127)) as u8]),
            Event(0, [0xC0 | ch, program as u8, 0]),
            Event(1, [0x90 | ch, 60, 100]),
            Event(TICKS_PER_SECOND / 2, [0x80 | ch, 60, 0]),
        ];
        let out = render(&sf, &midi(events), 1.0);
        assert!(
            out.left.iter().chain(&out.right).all(|s| s.is_finite()),
            "preset {bank}:{program}"
        );
    }
}
