//! Stereo input and output telemetry and editor-side spectrum analysis.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub(crate) const WINDOW_SAMPLES: usize = 4096;
const HOP_SAMPLES: usize = WINDOW_SAMPLES / 2;
type SpectrumWindow = [[u64; 2]; WINDOW_SAMPLES];

/// One audio producer and one editor consumer share a single completed window.
pub(crate) struct SpectrumInput {
    // The low bit enables capture. Advancing the generation invalidates old audio.
    generation: AtomicU64,
    ready: AtomicBool,
    window_generation: AtomicU64,
    sample_rate: AtomicU32,
    samples: Box<[[AtomicU64; 2]]>,
}

impl Default for SpectrumInput {
    fn default() -> Self {
        Self {
            generation: AtomicU64::new(0),
            ready: AtomicBool::new(false),
            window_generation: AtomicU64::new(0),
            sample_rate: AtomicU32::new(0),
            samples: (0..WINDOW_SAMPLES).map(|_| Default::default()).collect(),
        }
    }
}

impl SpectrumInput {
    pub(crate) fn set_enabled(&self, enabled: bool) {
        let bit = u64::from(enabled);
        let _ = self
            .generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                (generation & 1 != bit).then_some(((generation & !1).wrapping_add(2)) | bit)
            });
    }

    pub(crate) fn invalidate(&self) {
        self.generation.fetch_add(2, Ordering::Release);
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn publish(&self, generation: u64, sample_rate: f32, samples: &SpectrumWindow) {
        if self.ready.load(Ordering::Acquire) || self.generation() != generation {
            return;
        }
        for (slots, &[pre, post]) in self.samples.iter().zip(samples) {
            slots[0].store(pre, Ordering::Relaxed);
            slots[1].store(post, Ordering::Relaxed);
        }
        self.sample_rate
            .store(sample_rate.to_bits(), Ordering::Relaxed);
        self.window_generation.store(generation, Ordering::Relaxed);
        self.ready.store(true, Ordering::Release);
    }

    #[cfg(any(
        test,
        all(feature = "gpui-gui", any(target_os = "macos", target_os = "windows"))
    ))]
    pub(crate) fn read(&self, samples: &mut SpectrumWindow) -> Option<f32> {
        if !self.ready.load(Ordering::Acquire) {
            return None;
        }
        // The producer will skip publication until the reader releases this slot.
        let generation = self.window_generation.load(Ordering::Relaxed);
        let rate = f32::from_bits(self.sample_rate.load(Ordering::Relaxed));
        let current = generation == self.generation();
        if current {
            for (sample, [pre, post]) in samples.iter_mut().zip(&self.samples) {
                *sample = [pre.load(Ordering::Relaxed), post.load(Ordering::Relaxed)];
            }
        }
        self.ready.store(false, Ordering::Release);
        (current && generation == self.generation()).then_some(rate)
    }
}

/// Collects aligned, overlapping windows with no allocations or FFT work in processing.
pub(crate) struct SpectrumWriter {
    generation: u64,
    sample_rate: f32,
    filled: usize,
    samples: Box<SpectrumWindow>,
}

impl Default for SpectrumWriter {
    fn default() -> Self {
        Self {
            generation: 0,
            sample_rate: 0.0,
            filled: 0,
            samples: Box::new([[0; 2]; WINDOW_SAMPLES]),
        }
    }
}

impl SpectrumWriter {
    pub(crate) fn begin_block(&mut self, input: &SpectrumInput, sample_rate: f32) -> bool {
        let generation = input.generation();
        if generation != self.generation || sample_rate != self.sample_rate {
            self.filled = 0;
            self.generation = generation;
            self.sample_rate = sample_rate;
        }
        generation & 1 != 0 && sample_rate.is_finite() && sample_rate > 0.0
    }

    pub(crate) fn record(&mut self, input: &SpectrumInput, pre: [f32; 2], post: [f32; 2]) {
        let pack = |[left, right]: [f32; 2]| {
            u64::from(left.to_bits()) | (u64::from(right.to_bits()) << 32)
        };
        self.samples[self.filled] = [pack(pre), pack(post)];
        self.filled += 1;
        if self.filled == WINDOW_SAMPLES {
            input.publish(self.generation, self.sample_rate, &self.samples);
            self.samples.copy_within(HOP_SAMPLES.., 0);
            self.filled = WINDOW_SAMPLES - HOP_SAMPLES;
        }
    }

    pub(crate) fn reset(&mut self) {
        self.filled = 0;
    }
}

#[cfg(any(
    test,
    all(feature = "gpui-gui", any(target_os = "macos", target_os = "windows"))
))]
mod analyzer {
    use std::sync::Arc;

    use rustfft::{num_complex::Complex32, Fft, FftPlanner};

    use super::{SpectrumInput, SpectrumWindow, WINDOW_SAMPLES};
    use crate::dual_band::{CROSSOVER_INDEX, MAX, MIN};

    const PLOT_POINTS: usize = 128;
    const FLOOR_DB: f32 = -84.0;
    const STALE_MICROS: u64 = 200_000;
    const AMPLITUDE_SCALE: f32 = 4.0 / WINDOW_SAMPLES as f32;

    pub(crate) type SpectrumTrace = [f32; PLOT_POINTS];

    #[derive(Clone, Copy)]
    pub(crate) struct SpectrumTraces {
        pub(crate) pre: SpectrumTrace,
        pub(crate) post: SpectrumTrace,
    }

    pub(crate) struct SpectrumAnalyzer {
        fft: Arc<dyn Fft<f32>>,
        scratch: Vec<Complex32>,
        transform: Vec<Complex32>,
        hann: Vec<f32>,
        power: Vec<f32>,
        samples: Box<SpectrumWindow>,
        target_db: [SpectrumTrace; 2],
        displayed_db: [SpectrumTrace; 2],
        generation: u64,
        last_sample: Option<u64>,
        last_frame: u64,
    }

    impl Default for SpectrumAnalyzer {
        fn default() -> Self {
            let fft = FftPlanner::new().plan_fft_forward(WINDOW_SAMPLES);
            Self {
                scratch: vec![Complex32::default(); fft.get_inplace_scratch_len()],
                fft,
                transform: vec![Complex32::default(); WINDOW_SAMPLES],
                hann: (0..WINDOW_SAMPLES)
                    .map(|index| {
                        let phase = std::f32::consts::TAU * index as f32 / WINDOW_SAMPLES as f32;
                        0.5 - 0.5 * phase.cos()
                    })
                    .collect(),
                power: vec![0.0; WINDOW_SAMPLES / 2 + 1],
                samples: Box::new([[0; 2]; WINDOW_SAMPLES]),
                target_db: [[FLOOR_DB; PLOT_POINTS]; 2],
                displayed_db: [[FLOOR_DB; PLOT_POINTS]; 2],
                generation: 0,
                last_sample: None,
                last_frame: 0,
            }
        }
    }

    impl SpectrumAnalyzer {
        /// Return line heights on the log-frequency axis, with a short release.
        pub(crate) fn update(&mut self, input: &SpectrumInput, now: u64) -> Option<SpectrumTraces> {
            let generation = input.generation();
            if generation != self.generation {
                self.generation = generation;
                self.last_sample = None;
                self.displayed_db.fill([FLOOR_DB; PLOT_POINTS]);
            }
            if let Some(sample_rate) = input.read(&mut self.samples) {
                self.analyze(sample_rate);
                self.last_sample = Some(now);
            }
            if self
                .last_sample
                .is_none_or(|sample| now.saturating_sub(sample) > STALE_MICROS)
            {
                self.target_db.fill([FLOOR_DB; PLOT_POINTS]);
            }
            let seconds = now.saturating_sub(self.last_frame) as f32 / 1_000_000.0;
            self.last_frame = now;
            let release = (-seconds / 0.14).exp();
            for (displayed, &target) in self
                .displayed_db
                .iter_mut()
                .flatten()
                .zip(self.target_db.iter().flatten())
            {
                *displayed = target.max(target + (*displayed - target) * release);
            }
            self.displayed_db
                .iter()
                .flatten()
                .any(|&db| db > FLOOR_DB + 1.0)
                .then(|| {
                    let normalize =
                        |trace: SpectrumTrace| trace.map(|db| (db - FLOOR_DB) / -FLOOR_DB);
                    SpectrumTraces {
                        pre: normalize(self.displayed_db[0]),
                        post: normalize(self.displayed_db[1]),
                    }
                })
        }

        fn analyze(&mut self, sample_rate: f32) {
            for (stage, target_db) in self.target_db.iter_mut().enumerate() {
                self.power.fill(0.0);
                for shift in [0, 32] {
                    for ((sample, pair), &window) in self
                        .transform
                        .iter_mut()
                        .zip(self.samples.iter())
                        .zip(&self.hann)
                    {
                        let value = f32::from_bits((pair[stage] >> shift) as u32);
                        let value = if value.is_finite() {
                            value.clamp(-16.0, 16.0)
                        } else {
                            0.0
                        };
                        *sample = Complex32::new(value * window * AMPLITUDE_SCALE, 0.0);
                    }
                    self.fft
                        .process_with_scratch(&mut self.transform, &mut self.scratch);
                    for (total, sample) in self.power.iter_mut().zip(&self.transform) {
                        *total += sample.norm_sqr() * 0.5;
                    }
                }
                for (index, target) in target_db.iter_mut().enumerate() {
                    let center = fft_bin_at_plot_point(index as f32, sample_rate);
                    if center > (self.power.len() - 1) as f32 {
                        *target = FLOOR_DB;
                        continue;
                    }
                    // Interpolate between bins at the low end. At the high end,
                    // preserve any narrow peak inside the wider log-frequency bucket.
                    let bin = (center as usize).max(1);
                    let next = (bin + 1).min(self.power.len() - 1);
                    let mix = (center - bin as f32).max(0.0);
                    let mut peak = self.power[bin] * (1.0 - mix) + self.power[next] * mix;
                    let first = (fft_bin_at_plot_point(index as f32 - 0.5, sample_rate).ceil()
                        as usize)
                        .max(1);
                    let last = (fft_bin_at_plot_point(index as f32 + 0.5, sample_rate).floor()
                        as usize)
                        .min(self.power.len() - 1);
                    if first <= last {
                        for &power in &self.power[first..=last] {
                            peak = peak.max(power);
                        }
                    }
                    *target = (10.0 * peak.log10()).clamp(FLOOR_DB, 0.0);
                }
            }
        }
    }

    pub(super) fn fft_bin_at_plot_point(index: f32, sample_rate: f32) -> f32 {
        let min = MIN[CROSSOVER_INDEX];
        let max = MAX[CROSSOVER_INDEX];
        let phase = index / (PLOT_POINTS - 1) as f32;
        (min * (max / min).powf(phase)) * WINDOW_SAMPLES as f32 / sample_rate
    }
}

#[cfg(any(
    test,
    all(feature = "gpui-gui", any(target_os = "macos", target_os = "windows"))
))]
pub(crate) use analyzer::SpectrumAnalyzer;
#[cfg(all(feature = "gpui-gui", any(target_os = "macos", target_os = "windows")))]
pub(crate) use analyzer::{SpectrumTrace, SpectrumTraces};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_alloc::assert_no_alloc;

    #[test]
    fn track_spectrum_detects_antiphase_stereo_with_actual_sample_rate() {
        for rate in [48_000.0, 96_000.0] {
            let input = SpectrumInput::default();
            let mut writer = SpectrumWriter::default();
            let mut analyzer = SpectrumAnalyzer::default();
            assert!(!writer.begin_block(&input, rate));
            input.set_enabled(true);
            assert!(writer.begin_block(&input, rate));
            let tone = 937.5;
            assert_no_alloc(|| {
                for index in 0..WINDOW_SAMPLES {
                    let value = (std::f32::consts::TAU * tone * index as f32 / rate).sin() * 0.5;
                    writer.record(&input, [value, -value], [value * 0.5, -value * 0.5]);
                }
            });
            let trace = analyzer.update(&input, 1_000_000).unwrap();
            let (index, &peak) = trace
                .pre
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .unwrap();
            let hz =
                analyzer::fft_bin_at_plot_point(index as f32, rate) * rate / WINDOW_SAMPLES as f32;
            assert!((hz - tone).abs() < 40.0, "{hz}");
            assert!(peak > 0.90 && peak < 0.94, "{peak}");
            assert!(trace.pre[0] < 0.03);
            assert!((peak - trace.post[index] - 6.0206 / 84.0).abs() < 0.002);
            assert!(trace.pre[trace.pre.len() - 1] < 0.03);
            let decaying = analyzer.update(&input, 1_400_000).unwrap();
            assert!(decaying.pre[index] < 0.1);
            assert!(analyzer.update(&input, 2_000_000).is_none());
        }
    }

    #[test]
    fn track_spectrum_discards_partial_and_published_windows_on_invalidation() {
        let input = SpectrumInput::default();
        let mut writer = SpectrumWriter::default();
        let mut samples = Box::new([[0; 2]; WINDOW_SAMPLES]);
        input.set_enabled(true);
        assert!(writer.begin_block(&input, 48_000.0));
        for _ in 0..WINDOW_SAMPLES {
            writer.record(&input, [1.0; 2], [0.0; 2]);
        }
        input.set_enabled(false);
        assert!(!writer.begin_block(&input, 48_000.0));
        input.set_enabled(true);
        assert!(writer.begin_block(&input, 96_000.0));
        assert_eq!(input.read(&mut samples), None);
        for _ in 0..HOP_SAMPLES {
            writer.record(&input, [0.5; 2], [0.0; 2]);
        }
        input.invalidate();
        assert!(writer.begin_block(&input, 96_000.0));
        for _ in 0..HOP_SAMPLES {
            writer.record(&input, [0.0; 2], [0.0; 2]);
        }
        assert_eq!(input.read(&mut samples), None);
        for _ in 0..HOP_SAMPLES {
            writer.record(&input, [0.0; 2], [0.0; 2]);
        }
        assert_eq!(input.read(&mut samples), Some(96_000.0));
        assert!(samples.iter().all(|&sample| sample == [0; 2]));
        for _ in 0..HOP_SAMPLES {
            writer.record(&input, [0.0; 2], [0.0; 2]);
        }
        assert_eq!(input.read(&mut samples), Some(96_000.0));
    }
}
