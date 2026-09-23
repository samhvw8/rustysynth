//! Builders for synthetic SoundFonts and MIDI files, and render helpers shared by the
//! behaviour tests. Real soundfont fixtures come from `fetch-test-soundfonts.sh`.

use rustysynth::{MidiFile, MidiFileSequencer, SoundFont, Synthesizer, SynthesizerSettings};
use std::fs::File;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

pub const SAMPLE_RATE: i32 = 44_100;

pub fn fixture(name: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.push(name);
    assert!(
        path.exists(),
        "{} is missing; run ./fetch-test-soundfonts.sh at the repository root",
        path.display()
    );
    path
}

pub fn load(name: &str) -> Arc<SoundFont> {
    Arc::new(SoundFont::new(&mut File::open(fixture(name)).unwrap()).unwrap())
}

fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn list(kind: &[u8; 4], chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut body = kind.to_vec();
    for c in chunks {
        body.extend_from_slice(c);
    }
    chunk(b"LIST", &body)
}

fn name20(name: &str) -> Vec<u8> {
    let mut out = name.as_bytes().to_vec();
    out.resize(20, 0);
    out
}

fn u16s(values: &[u16]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// Loop points and mode of the single region in a synthetic SoundFont.
pub struct Region {
    /// SF2 `sampleModes`: 0 no loop, 1 continuous, 3 loop until note-off.
    pub sample_modes: u16,
    pub start_loop: u32,
    pub end_loop: u32,
    /// Sample end reported by the header; defaults to the real length when `None`.
    pub end: Option<u32>,
}

/// A one-preset, one-instrument, one-sample SoundFont (bank 0, program 0) whose sample is a sine at
/// A4 (440 Hz, root key 69) of `frames` samples, followed by the 46 zero samples SF2 requires.
pub fn synthetic_sf2(frames: u32, region: &Region) -> Vec<u8> {
    let mut wave: Vec<i16> = (0..frames)
        .map(|i| {
            ((i as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE as f32).sin() * 16_000.0)
                as i16
        })
        .collect();
    wave.extend(std::iter::repeat_n(0, 46));
    let smpl: Vec<u8> = wave.iter().flat_map(|s| s.to_le_bytes()).collect();

    let info = list(
        b"INFO",
        &[
            chunk(b"ifil", &u16s(&[2, 1])),
            chunk(b"isng", b"EMU8000\0"),
            chunk(b"INAM", b"synthetic\0"),
        ],
    );
    let sdta = list(b"sdta", &[chunk(b"smpl", &smpl)]);

    let mut phdr = name20("preset");
    phdr.extend(u16s(&[0, 0, 0]));
    phdr.extend([0; 12]);
    phdr.extend(name20("EOP"));
    phdr.extend(u16s(&[0, 0, 1]));
    phdr.extend([0; 12]);

    const INSTRUMENT: u16 = 41;
    const SAMPLE_MODES: u16 = 54;
    const SAMPLE_ID: u16 = 53;
    let mut inst = name20("instrument");
    inst.extend(u16s(&[0]));
    inst.extend(name20("EOI"));
    inst.extend(u16s(&[1]));

    let mut shdr = name20("sine");
    let end = region.end.unwrap_or(frames);
    for v in [
        0,
        end,
        region.start_loop,
        region.end_loop,
        SAMPLE_RATE as u32,
    ] {
        shdr.extend(v.to_le_bytes());
    }
    shdr.extend([69, 0]);
    shdr.extend(u16s(&[0, 1]));
    shdr.extend(name20("EOS"));
    shdr.extend([0; 26]);

    let pdta = list(
        b"pdta",
        &[
            chunk(b"phdr", &phdr),
            chunk(b"pbag", &u16s(&[0, 0, 1, 0])),
            chunk(b"pmod", &[0; 10]),
            chunk(b"pgen", &u16s(&[INSTRUMENT, 0, 0, 0])),
            chunk(b"inst", &inst),
            chunk(b"ibag", &u16s(&[0, 0, 2, 0])),
            chunk(b"imod", &[0; 10]),
            chunk(
                b"igen",
                &u16s(&[SAMPLE_MODES, region.sample_modes, SAMPLE_ID, 0, 0, 0]),
            ),
            chunk(b"shdr", &shdr),
        ],
    );

    let mut body = b"sfbk".to_vec();
    body.extend(info);
    body.extend(sdta);
    body.extend(pdta);
    chunk(b"RIFF", &body)
}

/// A MIDI event at an absolute tick (480 ticks per quarter note, 120 bpm: 960 ticks per second).
pub struct Event(pub u32, pub [u8; 3]);

pub const TICKS_PER_SECOND: u32 = 960;

/// A format-0 SMF from events, which need not be sorted.
pub fn midi(mut events: Vec<Event>) -> Vec<u8> {
    events.sort_by_key(|e| e.0);
    let mut track = vec![0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20];
    let mut now = 0;
    for Event(tick, bytes) in events {
        let mut delta = tick - now;
        now = tick;
        let mut vlq = vec![(delta & 0x7F) as u8];
        delta >>= 7;
        while delta > 0 {
            vlq.insert(0, 0x80 | (delta & 0x7F) as u8);
            delta >>= 7;
        }
        track.extend(vlq);
        let len = if bytes[0] & 0xF0 == 0xC0 { 2 } else { 3 };
        track.extend(&bytes[..len]);
    }
    track.extend([0x00, 0xFF, 0x2F, 0x00]);

    let mut out = b"MThd".to_vec();
    out.extend(6u32.to_be_bytes());
    out.extend(u16s(&[0, 1, 480]).chunks(2).flat_map(|b| [b[1], b[0]]));
    out.extend(b"MTrk");
    out.extend((track.len() as u32).to_be_bytes());
    out.extend(track);
    out
}

pub struct Stereo {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl Stereo {
    /// FNV-1a over the bit patterns of every sample: equal only for bit-identical renders.
    pub fn fingerprint(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for s in self.left.iter().chain(&self.right) {
            for b in s.to_bits().to_le_bytes() {
                hash = (hash ^ b as u64).wrapping_mul(0x0100_0000_01b3);
            }
        }
        hash
    }

    /// Mean square of both channels over the frames in `[from, to)` seconds.
    pub fn energy(&self, from: f64, to: f64) -> f64 {
        let a = (from * SAMPLE_RATE as f64) as usize;
        let b = ((to * SAMPLE_RATE as f64) as usize).min(self.left.len());
        let sum: f64 = (a..b)
            .map(|i| (self.left[i] as f64).powi(2) + (self.right[i] as f64).powi(2))
            .sum();
        sum / (2 * (b - a)) as f64
    }

    pub fn peak(&self) -> f32 {
        self.left
            .iter()
            .chain(&self.right)
            .fold(0_f32, |m, s| m.max(s.abs()))
    }
}

/// Renders `midi` the way audio-worker does: MidiFileSequencer, 44.1 kHz, default settings.
pub fn render(sound_font: &Arc<SoundFont>, midi: &[u8], seconds: f64) -> Stereo {
    let midi = Arc::new(MidiFile::new(&mut Cursor::new(midi)).unwrap());
    let synth = Synthesizer::new(sound_font, &SynthesizerSettings::new(SAMPLE_RATE)).unwrap();
    let mut sequencer = MidiFileSequencer::new(synth);
    sequencer.play(&midi, false);
    let frames = (seconds * SAMPLE_RATE as f64) as usize;
    let mut out = Stereo {
        left: vec![0_f32; frames],
        right: vec![0_f32; frames],
    };
    sequencer.render(&mut out.left, &mut out.right);
    out
}

/// Six seconds that exercise what scores use: several GM programs on different channels, chords
/// dense enough to hit the 64-voice limit (voice stealing), drums, sustain pedal, pitch bend,
/// vibrato, pan, volume, expression, reverb and chorus sends.
pub fn ensemble_midi() -> Vec<u8> {
    let s = TICKS_PER_SECOND;
    let mut e = Vec::new();
    for (ch, program) in [(0u8, 0u8), (1, 48), (2, 73), (3, 32), (4, 56)] {
        e.push(Event(0, [0xC0 | ch, program, 0]));
        e.push(Event(0, [0xB0 | ch, 91, 40 + ch * 15]));
        e.push(Event(0, [0xB0 | ch, 93, ch * 20]));
        e.push(Event(0, [0xB0 | ch, 10, 16 + ch * 24]));
    }
    // Piano: arpeggiated chords under the sustain pedal, then released.
    e.push(Event(0, [0xB0, 64, 127]));
    for (i, key) in [48u8, 52, 55, 60, 64, 67, 72, 76, 79, 84]
        .iter()
        .enumerate()
    {
        let t = i as u32 * s / 8;
        e.push(Event(t, [0x90, *key, 60 + i as u8 * 6]));
        e.push(Event(t + s / 4, [0x80, *key, 0]));
    }
    e.push(Event(3 * s, [0xB0, 64, 0]));
    // Strings: a sustained cluster, with a volume swell through expression.
    for key in 55..79u8 {
        e.push(Event(s / 2, [0x91, key, 70]));
        e.push(Event(4 * s, [0x81, key, 0]));
    }
    for step in 0..20 {
        e.push(Event(
            s / 2 + step * s / 10,
            [0xB1, 11, 40 + step as u8 * 4],
        ));
    }
    // Flute melody with vibrato and a pitch bend up and back.
    e.push(Event(s, [0xB2, 1, 80]));
    for (i, key) in [72u8, 74, 76, 77, 79, 81, 83, 84].iter().enumerate() {
        let t = s + i as u32 * s / 3;
        e.push(Event(t, [0x92, *key, 90]));
        e.push(Event(t + s / 3, [0x82, *key, 0]));
    }
    for step in 0..=16u32 {
        let bend = 8192 + (step.min(16 - step) * 256);
        e.push(Event(
            2 * s + step * s / 32,
            [0xE2, (bend & 0x7F) as u8, (bend >> 7) as u8],
        ));
    }
    // Bass and brass stabs at lowered channel volume.
    e.push(Event(0, [0xB3, 7, 90]));
    e.push(Event(0, [0xB4, 7, 70]));
    for beat in 0..8u32 {
        let t = beat * s / 2;
        e.push(Event(t, [0x93, 36 + (beat % 4) as u8 * 2, 100]));
        e.push(Event(t + s / 3, [0x83, 36 + (beat % 4) as u8 * 2, 0]));
        e.push(Event(t + s / 4, [0x94, 60, 80]));
        e.push(Event(t + s / 4, [0x94, 64, 80]));
        e.push(Event(t + s / 2, [0x84, 60, 0]));
        e.push(Event(t + s / 2, [0x84, 64, 0]));
    }
    // Drums on channel 10: kick, snare, hi-hat.
    for step in 0..32u32 {
        let t = step * s / 8;
        e.push(Event(t, [0x99, 42, 70]));
        if step % 4 == 0 {
            e.push(Event(t, [0x99, 36, 110]));
        }
        if step % 8 == 4 {
            e.push(Event(t, [0x99, 38, 100]));
        }
    }
    e.push(Event(6 * s, [0xB0, 121, 0]));
    midi(e)
}
