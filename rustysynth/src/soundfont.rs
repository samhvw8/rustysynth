#![allow(dead_code)]

use std::io::Read;

use crate::binary_reader::BinaryReader;
use crate::error::SoundFontError;
use crate::four_cc::FourCC;
use crate::generator_type::GeneratorType;
use crate::instrument::Instrument;
use crate::preset::Preset;
use crate::sample_header::SampleHeader;
use crate::soundfont_info::SoundFontInfo;
use crate::soundfont_parameters::SoundFontParameters;
use crate::soundfont_sampledata::SoundFontSampleData;
use crate::LoopMode;

/// Reperesents a SoundFont.
#[derive(Debug)]
#[non_exhaustive]
pub struct SoundFont {
    pub(crate) info: SoundFontInfo,
    pub(crate) bits_per_sample: i32,
    pub(crate) wave_data: WaveData,
    pub(crate) sample_headers: Vec<SampleHeader>,
    pub(crate) presets: Vec<Preset>,
    pub(crate) instruments: Vec<Instrument>,
}

impl SoundFont {
    /// Loads a SoundFont from the stream.
    ///
    /// # Arguments
    ///
    /// * `reader` - The data stream used to load the SoundFont.
    pub fn new<R: Read>(reader: &mut R) -> Result<Self, SoundFontError> {
        let chunk_id = BinaryReader::read_four_cc(reader)?;
        if chunk_id != b"RIFF" {
            return Err(SoundFontError::RiffChunkNotFound);
        }

        let _size = BinaryReader::read_i32(reader)?;

        let form_type = BinaryReader::read_four_cc(reader)?;
        if form_type != b"sfbk" {
            return Err(SoundFontError::InvalidRiffChunkType {
                expected: FourCC::from_bytes(*b"sfbk"),
                actual: form_type,
            });
        }

        let info = SoundFontInfo::new(reader)?;
        let sample_data = SoundFontSampleData::new(reader)?;
        let parameters = SoundFontParameters::new(reader)?;

        let mut sound_font = Self {
            info,
            bits_per_sample: sample_data.bits_per_sample,
            wave_data: WaveData::Owned(sample_data.wave_data),
            sample_headers: parameters.sample_headers,
            presets: parameters.presets,
            instruments: parameters.instruments,
        };

        sound_font.sanity_check()?;

        Ok(sound_font)
    }

    /// Loads a SoundFont whose sample data stays in the file, memory-mapped, instead of being copied
    /// into memory. Only the pages that playback reads become resident, and the operating system can
    /// drop them again under memory pressure. The file must not be modified while the SoundFont lives.
    pub fn new_mmap(file: &std::fs::File) -> Result<Self, SoundFontError> {
        let map = unsafe { memmap2::Mmap::map(file) }?;
        let bytes: &[u8] = &map;
        let u32_at = |at: usize| -> Result<usize, SoundFontError> {
            let b = bytes
                .get(at..at + 4)
                .ok_or(SoundFontError::SampleDataNotFound)?;
            Ok(u32::from_le_bytes(b.try_into().unwrap()) as usize)
        };
        if bytes.get(0..4) != Some(b"RIFF") {
            return Err(SoundFontError::RiffChunkNotFound);
        }
        if bytes.get(8..12) != Some(b"sfbk") {
            return Err(SoundFontError::InvalidRiffChunkType {
                expected: FourCC::from_bytes(*b"sfbk"),
                actual: FourCC::from_bytes(bytes[8..12].try_into().unwrap()),
            });
        }

        // Top-level LIST chunks: INFO, sdta and pdta, each "LIST" + size + type.
        let (mut info_at, mut sdta_at, mut pdta_at) = (None, None, None);
        let mut at = 12;
        while at + 12 <= bytes.len() {
            let size = u32_at(at + 4)?;
            if &bytes[at..at + 4] == b"LIST" {
                match &bytes[at + 8..at + 12] {
                    b"INFO" => info_at = Some(at),
                    b"sdta" => sdta_at = Some((at, size)),
                    b"pdta" => pdta_at = Some(at),
                    _ => {}
                }
            }
            at += 8 + size + (size & 1);
        }
        let (Some(info_at), Some((sdta_at, sdta_size)), Some(pdta_at)) =
            (info_at, sdta_at, pdta_at)
        else {
            return Err(SoundFontError::ListChunkNotFound);
        };

        // Inside sdta: the smpl chunk (16-bit samples); sm24 is ignored as in `new`.
        let mut wave = None;
        let mut at = sdta_at + 12;
        while at + 8 <= sdta_at + 8 + sdta_size {
            let size = u32_at(at + 4)?;
            if &bytes[at..at + 4] == b"smpl" {
                wave = Some((at + 8, size / 2));
            }
            at += 8 + size + (size & 1);
        }
        let Some((offset, len)) = wave else {
            return Err(SoundFontError::SampleDataNotFound);
        };
        if len < 2 || offset + len * 2 > bytes.len() {
            return Err(SoundFontError::SampleDataNotFound);
        }
        if &bytes[offset..offset + 4] == b"OggS" {
            return Err(SoundFontError::UnsupportedSampleFormat);
        }

        let info = SoundFontInfo::new(&mut std::io::Cursor::new(&bytes[info_at..]))?;
        let parameters = SoundFontParameters::new(&mut std::io::Cursor::new(&bytes[pdta_at..]))?;
        let wave_data = if cfg!(target_endian = "little") && offset % 2 == 0 {
            WaveData::Mapped { map, offset, len }
        } else {
            let copied = bytes[offset..offset + len * 2]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b))
                .collect();
            WaveData::Owned(copied)
        };

        let mut sound_font = Self {
            info,
            bits_per_sample: 16,
            wave_data,
            sample_headers: parameters.sample_headers,
            presets: parameters.presets,
            instruments: parameters.instruments,
        };
        sound_font.sanity_check()?;
        Ok(sound_font)
    }

    // Patch from https://github.com/sinshu/rustysynth/issues/55: reject only an
    // unusable playback range; repair an unusable loop range by disabling the loop.
    fn sanity_check(&mut self) -> Result<(), SoundFontError> {
        let wave_length = self.wave_data.len();
        for instrument in &mut self.instruments {
            for region in &mut instrument.regions {
                let start = region.get_sample_start();
                let end = region.get_sample_end();
                if start < 0 || end <= start || end as usize >= wave_length {
                    return Err(SoundFontError::SanityCheckFailed);
                }
                if region.get_sample_modes() == LoopMode::NoLoop {
                    continue;
                }
                let start_loop = region.get_sample_start_loop();
                let end_loop = region.get_sample_end_loop();
                if start_loop < 0 || end_loop as usize >= wave_length || start_loop >= end_loop {
                    region.gs[GeneratorType::SAMPLE_MODES as usize] = 0;
                }
            }
        }
        Ok(())
    }

    /// Gets the information of the SoundFont.
    pub fn get_info(&self) -> &SoundFontInfo {
        &self.info
    }

    /// Gets the bits per sample of the sample data.
    pub fn get_bits_per_sample(&self) -> i32 {
        self.bits_per_sample
    }

    /// Gets the sample data.
    pub fn get_wave_data(&self) -> &[i16] {
        &self.wave_data[..]
    }

    /// Gets the samples of the SoundFont.
    pub fn get_sample_headers(&self) -> &[SampleHeader] {
        &self.sample_headers[..]
    }

    /// Gets the presets of the SoundFont.
    pub fn get_presets(&self) -> &[Preset] {
        &self.presets[..]
    }

    /// Gets the instruments of the SoundFont.
    pub fn get_instruments(&self) -> &[Instrument] {
        &self.instruments[..]
    }
}

/// Sample data: read into memory, or a view into a memory-mapped file.
pub(crate) enum WaveData {
    Owned(Vec<i16>),
    Mapped {
        map: memmap2::Mmap,
        offset: usize,
        len: usize,
    },
}

impl std::ops::Deref for WaveData {
    type Target = [i16];

    fn deref(&self) -> &[i16] {
        match self {
            WaveData::Owned(samples) => samples,
            // SAFETY: new_mmap checked bounds, 2-byte alignment and little-endian byte order.
            WaveData::Mapped { map, offset, len } => unsafe {
                std::slice::from_raw_parts(map.as_ptr().add(*offset) as *const i16, *len)
            },
        }
    }
}

impl std::fmt::Debug for WaveData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = if matches!(self, WaveData::Owned(_)) {
            "owned"
        } else {
            "mapped"
        };
        write!(f, "WaveData({kind}, {} samples)", self.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{fs::File, path::PathBuf};

    fn samples_dir_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("samples")
    }

    #[test]
    fn test_load_reject_sf3() {
        let path = samples_dir_path().join("dummy.sf3");
        let mut file = File::open(&path).unwrap();
        assert!(matches!(
            SoundFont::new(&mut file),
            Err(SoundFontError::UnsupportedSampleFormat)
        ));
    }

    #[test]
    fn mmap_rejects_what_new_rejects() {
        let file = File::open(samples_dir_path().join("dummy.sf3")).unwrap();
        assert!(matches!(
            SoundFont::new_mmap(&file),
            Err(SoundFontError::UnsupportedSampleFormat)
        ));
        let file = File::open(samples_dir_path().join("test_empty_samples.sf2")).unwrap();
        assert!(matches!(
            SoundFont::new_mmap(&file),
            Err(SoundFontError::SampleDataNotFound)
        ));
    }

    #[test]
    fn mmap_loads_the_same_soundfont_as_new() {
        let root = samples_dir_path().parent().unwrap().to_path_buf();
        for name in ["TimGM6mb.sf2", "GeneralUser GS MuseScore v1.442.sf2"] {
            let path = root.join(name);
            if !path.exists() {
                eprintln!("skipped {name}: run ./fetch-test-soundfonts.sh");
                continue;
            }
            let read = SoundFont::new(&mut File::open(&path).unwrap()).unwrap();
            let mapped = SoundFont::new_mmap(&File::open(&path).unwrap()).unwrap();
            assert!(
                matches!(mapped.wave_data, WaveData::Mapped { .. }),
                "{name}"
            );
            assert_eq!(read.get_wave_data(), mapped.get_wave_data(), "{name}");
            assert_eq!(
                format!("{:?}", read.info),
                format!("{:?}", mapped.info),
                "{name}"
            );
            assert_eq!(
                format!("{:?}", read.presets),
                format!("{:?}", mapped.presets),
                "{name}"
            );
            assert_eq!(
                format!("{:?}", read.instruments),
                format!("{:?}", mapped.instruments),
                "{name}"
            );
            assert_eq!(
                format!("{:?}", read.sample_headers),
                format!("{:?}", mapped.sample_headers),
                "{name}"
            );
        }
    }

    // smpl sub-chunk exists, but is zero-length.
    #[test]
    fn test_load_empty_samples() {
        let path = samples_dir_path().join("test_empty_samples.sf2");
        let mut file = File::open(&path).unwrap();
        assert!(matches!(
            SoundFont::new(&mut file),
            Err(SoundFontError::SampleDataNotFound)
        ));
    }
}
