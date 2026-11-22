//! One-cycle gain capture owned by the audio thread.

/// Endpoints included; the completed snapshot is suitable for curve fitting.
pub(crate) const CAPTURE_POINTS: usize = 32769;
pub(crate) const MAX_CAPTURE_GAIN: f32 = 16.0;
const MAX_PHASE_STEP: f32 = 0.1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Waiting,
    Recording,
    Complete,
}

/// Captures one complete forward cycle after arming, without allocating.
pub(crate) struct CompressorCapture {
    state: State,
    previous: Option<(f32, f32)>,
    points: Box<[f32]>,
    next_point: usize,
}

impl CompressorCapture {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Waiting,
            previous: None,
            points: vec![1.0; CAPTURE_POINTS].into_boxed_slice(),
            next_point: 0,
        }
    }

    #[cfg(feature = "vst3")]
    fn empty() -> Self {
        Self {
            state: State::Waiting,
            previous: None,
            points: Box::default(),
            next_point: 0,
        }
    }

    /// Rearm after cancellation, a transport discontinuity, or a completed take.
    pub(crate) fn arm(&mut self) {
        self.state = State::Waiting;
        self.previous = None;
        self.next_point = 0;
    }

    /// Feed a normalized, forward-running sync phase and linear detector gain.
    /// Call `arm` when transport or the cycle mapping changes.
    pub(crate) fn record(&mut self, phase: f32, gain: f32) {
        if self.state == State::Complete {
            return;
        }
        if !phase.is_finite() || !(0.0..1.0).contains(&phase) || !gain.is_finite() {
            self.arm();
            return;
        }
        let gain = gain.clamp(0.0, MAX_CAPTURE_GAIN);
        let Some((previous_phase, previous_gain)) = self.previous.replace((phase, gain)) else {
            return;
        };
        let wrapping = phase < previous_phase;
        let step = if wrapping {
            phase + 1.0 - previous_phase
        } else {
            phase - previous_phase
        };
        if step > MAX_PHASE_STEP {
            self.arm();
            self.previous = Some((phase, gain));
            return;
        }
        if step == 0.0 {
            return;
        }
        if wrapping {
            // Keep the two sides of the cycle cut independent. Interpolating
            // between them would smear a transient that begins at the boundary
            // backwards into the end of the captured cycle.
            if self.state == State::Recording {
                self.fill(previous_phase, previous_gain, 1.0, previous_gain);
                self.state = State::Complete;
                return;
            }
            self.state = State::Recording;
            self.points[0] = gain;
            self.next_point = 1;
            self.fill(0.0, gain, phase, gain);
        } else if self.state == State::Recording {
            self.fill(previous_phase, previous_gain, phase, gain);
        }
    }

    fn fill(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        while self.next_point < CAPTURE_POINTS {
            let x = self.next_point as f32 / (CAPTURE_POINTS - 1) as f32;
            if x > x1 {
                break;
            }
            let t = if x1 > x0 {
                ((x - x0) / (x1 - x0)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            self.points[self.next_point] = y0 + (y1 - y0) * t;
            self.next_point += 1;
        }
    }

    pub(crate) fn snapshot(&self) -> Option<&[f32]> {
        (self.state == State::Complete).then_some(&self.points)
    }
}

/// Audio-owned state for one transport-aligned capture request.
pub(crate) struct CaptureRunner {
    capture: CompressorCapture,
    request: u64,
    mapping: Option<[u32; 4]>,
    expected_beat: Option<f64>,
    valid_block: bool,
    complete: bool,
}

impl Default for CaptureRunner {
    fn default() -> Self {
        Self {
            capture: CompressorCapture::new(),
            request: 0,
            mapping: None,
            expected_beat: None,
            valid_block: false,
            complete: false,
        }
    }
}

fn mapping(settings: crate::dsp::DspSettings) -> [u32; 4] {
    [
        settings.beats_per_cycle.to_bits(),
        settings.delay_beats as u32,
        settings.swing.to_bits(),
        settings.phase_offset.to_bits(),
    ]
}

impl CaptureRunner {
    /// An allocation-free placeholder used while reconfiguring the audio engine.
    #[cfg(feature = "vst3")]
    pub(crate) fn empty() -> Self {
        Self {
            capture: CompressorCapture::empty(),
            request: 0,
            mapping: None,
            expected_beat: None,
            valid_block: false,
            complete: false,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.capture.arm();
        self.request = 0;
        self.mapping = None;
        self.expected_beat = None;
        self.valid_block = false;
        self.complete = false;
    }

    pub(crate) fn begin_block(
        &mut self,
        exchange: &CaptureExchange,
        settings: crate::dsp::DspSettings,
        transport: toybox::dsp::TransportState,
        frames: usize,
        sample_rate: f32,
    ) {
        let request = exchange.current_request();
        if request != self.request {
            self.capture.arm();
            self.expected_beat = None;
            self.mapping = None;
            self.complete = false;
            self.request = request;
        }
        let Some(beat) = transport.song_pos_beats.filter(|beat| beat.is_finite()) else {
            self.capture.arm();
            self.expected_beat = None;
            self.valid_block = false;
            return;
        };
        if request & 1 == 0
            || self.complete
            || !transport.is_playing
            || settings.timing_mode != crate::params::TIMING_MODE_SYNC
            || settings.delay_beats != 0
            || settings.compressor[0] < 0.5
            || settings.compressor[16] >= 0.5
            || settings.bypassed
            || !transport.tempo_bpm.is_finite()
            || transport.tempo_bpm <= 0.0
            || !sample_rate.is_finite()
            || sample_rate <= 0.0
        {
            self.capture.arm();
            self.expected_beat = None;
            self.valid_block = false;
            return;
        }
        let current_mapping = mapping(settings);
        let beat_per_sample = f64::from(transport.tempo_bpm) / (60.0 * f64::from(sample_rate));
        if self.mapping != Some(current_mapping)
            || self
                .expected_beat
                .is_some_and(|expected| (beat - expected).abs() > beat_per_sample * 4.0 + 1e-7)
        {
            self.capture.arm();
            self.mapping = Some(current_mapping);
        }
        self.expected_beat = Some(beat + frames as f64 * beat_per_sample);
        self.valid_block = true;
    }

    pub(crate) fn record(
        &mut self,
        exchange: &CaptureExchange,
        settings: crate::dsp::DspSettings,
        authored_phase: f32,
        gain: f32,
    ) {
        if !self.valid_block || self.complete || exchange.current_request() != self.request {
            return;
        }
        if settings.compressor[0] < 0.5
            || settings.delay_beats != 0
            || settings.compressor[16] >= 0.5
            || settings.bypassed
            || settings.timing_mode != crate::params::TIMING_MODE_SYNC
        {
            self.capture.arm();
            self.valid_block = false;
            return;
        }
        let current_mapping = mapping(settings);
        if self.mapping != Some(current_mapping) {
            self.capture.arm();
            self.mapping = Some(current_mapping);
        }
        self.capture.record(authored_phase, gain);
        if let Some(points) = self.capture.snapshot() {
            self.complete = exchange.publish(self.request, points);
        }
    }
}

/// Lock-free request and completed-take handoff between the editor and audio thread.
/// The editor is the sole requester; the audio processor is the sole publisher.
pub(crate) struct CaptureExchange {
    request: std::sync::atomic::AtomicU64,
    completed: std::sync::atomic::AtomicU64,
    sequence: std::sync::atomic::AtomicU64,
    points: Box<[std::sync::atomic::AtomicU32]>,
}

impl Default for CaptureExchange {
    fn default() -> Self {
        use std::sync::atomic::{AtomicU32, AtomicU64};
        Self {
            request: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
            points: (0..CAPTURE_POINTS)
                .map(|_| AtomicU32::new(1.0_f32.to_bits()))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        }
    }
}

impl CaptureExchange {
    /// Request a new take. A new request invalidates any earlier completion.
    pub(crate) fn request(&self) -> u64 {
        use std::sync::atomic::Ordering;
        let next = (self.request.load(Ordering::Relaxed) & !1).wrapping_add(2) | 1;
        self.request.store(next, Ordering::Release);
        next
    }

    /// Cancel the pending take and invalidate any completed snapshot.
    pub(crate) fn cancel(&self) {
        use std::sync::atomic::Ordering;
        let next = (self.request.load(Ordering::Relaxed) & !1).wrapping_add(2);
        self.request.store(next, Ordering::Release);
    }

    /// The processor observes this at block boundaries and rearms on change.
    pub(crate) fn current_request(&self) -> u64 {
        self.request.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Publish a completed take only for the still-current armed request.
    pub(crate) fn publish(&self, request: u64, points: &[f32]) -> bool {
        use std::sync::atomic::Ordering;
        if points.len() != CAPTURE_POINTS || request & 1 == 0 || self.current_request() != request {
            return false;
        }
        self.sequence.fetch_add(1, Ordering::AcqRel);
        for (slot, value) in self.points.iter().zip(points.iter()) {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
        self.sequence.fetch_add(1, Ordering::Release);
        if self.current_request() != request {
            return false;
        }
        self.completed.store(request, Ordering::Release);
        true
    }

    /// Read a complete, coherent take for the current request, if available.
    pub(crate) fn snapshot(&self) -> Option<Box<[f32]>> {
        use std::sync::atomic::Ordering;
        for _ in 0..3 {
            let request = self.current_request();
            if request & 1 == 0 || self.completed.load(Ordering::Acquire) != request {
                return None;
            }
            let before = self.sequence.load(Ordering::Acquire);
            if before & 1 != 0 {
                continue;
            }
            let points = self
                .points
                .iter()
                .map(|point| f32::from_bits(point.load(Ordering::Relaxed)))
                .collect::<Vec<_>>()
                .into_boxed_slice();
            let after = self.sequence.load(Ordering::Acquire);
            if before == after && self.current_request() == request {
                return Some(points);
            }
        }
        None
    }
}

const MAX_COMPRESSOR_ANCHORS: usize = 12;
const BEZIER_FIT_ERROR: f32 = 0.005;
const SIMPLIFICATION_ERROR: f32 = 0.016;
const SLOW_ENVELOPE_ERROR: f32 = 0.015;
const CLIPPED_START_PHASE: f32 = 0.000001;

#[derive(Clone, Copy)]
pub(crate) struct CompressorAnchor {
    pub(crate) sample: usize,
    pub(crate) gain: f32,
    pub(crate) tension: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FitBoundary {
    Open,
    Joined,
    Clipped,
}

/// Fit the main gain movement within a small editing budget. Adjust the
/// existing Bézier handles before spending another anchor.
pub(crate) fn fit_compressor_bezier(points: &[f32]) -> Vec<CompressorAnchor> {
    fit_bezier(points, FitBoundary::Open)
}

fn fit_bezier(points: &[f32], boundary: FitBoundary) -> Vec<CompressorAnchor> {
    let anchor = |sample| CompressorAnchor {
        sample,
        gain: points[sample],
        tension: 0.0,
    };
    if points.len() < 2 {
        return (0..points.len()).map(anchor).collect();
    }

    let mut anchors = vec![anchor(0), anchor(points.len() - 1)];
    loop {
        let mut greatest = (BEZIER_FIT_ERROR, 0, 0);
        for segment in 0..anchors.len() - 1 {
            let (tension, cubic) = fit_bezier_segment(points, &anchors, segment, boundary);
            let start = anchors[segment].sample;
            let end = anchors[segment + 1].sample;
            anchors[segment].tension = tension;
            if anchors.len() == MAX_COMPRESSOR_ANCHORS {
                continue;
            }
            for (index, &sample) in points.iter().enumerate().take(end).skip(start + 1) {
                if boundary == FitBoundary::Clipped && index == points.len() - 2 {
                    continue;
                }
                let error = (sample - cubic.sample(index as f32)).abs();
                if error > greatest.0 {
                    greatest = (error, segment + 1, index);
                }
            }
        }
        if greatest.0 <= BEZIER_FIT_ERROR {
            break;
        }
        anchors.insert(greatest.1, anchor(greatest.2));
    }
    simplify_compressor_anchors(points, anchors, boundary)
}

fn fit_bezier_segment(
    points: &[f32],
    anchors: &[CompressorAnchor],
    segment: usize,
    boundary: FitBoundary,
) -> (f32, crate::curve::CubicBezier) {
    use crate::curve::{CubicBezier, CurveNode};

    let last_sample = points.len() - 1;
    let cycle = last_sample as f32;
    let node = |anchor: CompressorAnchor| {
        let mut node = CurveNode {
            x: anchor.sample as f32,
            y: anchor.gain,
        };
        match boundary {
            FitBoundary::Clipped if anchor.sample == 0 => node.x = CLIPPED_START_PHASE * cycle,
            FitBoundary::Clipped if anchor.sample == last_sample => node.x = cycle - 1.0,
            FitBoundary::Joined if anchor.sample == last_sample => node.y = points[0],
            _ => {}
        }
        node
    };
    // Use the same neighbors at the cycle cut as the saved Bézier curve.
    // The live trace is open because its first and last bins may be incomplete.
    let previous = segment
        .checked_sub(1)
        .map(|index| node(anchors[index]))
        .or_else(|| {
            if boundary == FitBoundary::Open {
                return None;
            }
            let index = anchors.len()
                - if boundary == FitBoundary::Clipped {
                    1
                } else {
                    2
                };
            let mut previous = node(anchors[index]);
            previous.x -= cycle;
            Some(previous)
        });
    let next = anchors.get(segment + 2).copied().map(node).or_else(|| {
        if boundary == FitBoundary::Open {
            return None;
        }
        let index = usize::from(boundary == FitBoundary::Joined);
        let mut next = node(anchors[index]);
        next.x += cycle;
        Some(next)
    });
    let left = node(anchors[segment]);
    let right = node(anchors[segment + 1]);
    let cubic_at = |tension| CubicBezier::between(previous, left, right, next, tension);
    let start = anchors[segment].sample;
    let end = anchors[segment + 1].sample;
    let stride = (end - start).div_ceil(48);
    let mut best = (f32::INFINITY, 0.0, cubic_at(0.0));
    for tension in [0.0, -0.25, 0.25, -0.5, 0.5, -0.75, 0.75, -1.0, 1.0] {
        let cubic = cubic_at(tension);
        let error = ((start + 1)..end)
            .step_by(stride)
            .map(|index| (points[index] - cubic.sample(index as f32)).abs())
            .fold(0.0_f32, f32::max);
        if error < best.0 {
            best = (error, tension, cubic);
        }
    }
    (best.1, best.2)
}

/// Refit after removing a point. Measuring the gain averaged over a short
/// sliding window keeps important timing and recovery shape while forgiving
/// sample-level differences in an almost vertical attack.
fn simplify_compressor_anchors(
    points: &[f32],
    mut anchors: Vec<CompressorAnchor>,
    boundary: FitBoundary,
) -> Vec<CompressorAnchor> {
    if anchors.len() <= 2 {
        return anchors;
    }
    // Score timing at about 1.6% of a cycle. Protect genuine pulses that
    // return toward their starting level within about 0.1% on both sides.
    let window = points.len().div_ceil(64);
    let pulse_radius = points.len().div_ceil(1024);
    let isolated_pulses: Vec<_> = anchors
        .iter()
        .map(|anchor| anchor.sample)
        .filter(|&index| {
            let gain = points[index];
            let left = points[index.saturating_sub(pulse_radius)] - gain;
            let right = points[(index + pulse_radius).min(points.len() - 1)] - gain;
            left.min(right) >= 0.08 || left.max(right) <= -0.08
        })
        .collect();
    let mut recent = vec![0.0; window];
    let protected = |sample: usize| {
        isolated_pulses
            .iter()
            .any(|pulse| sample.abs_diff(*pulse) <= pulse_radius)
    };
    loop {
        let mut best_error = SIMPLIFICATION_ERROR;
        let mut best = None;
        for index in 1..anchors.len() - 1 {
            let middle = anchors[index];
            if protected(middle.sample) {
                continue;
            }
            let mut candidate = anchors.clone();
            candidate.remove(index);
            let error =
                simplification_error(points, &mut candidate, &mut recent, best_error, boundary);
            if error < best_error {
                best_error = error;
                best = Some(candidate.clone());
            }

            let left = anchors[index - 1];
            let next = anchors[index + 1];
            let entry = middle.gain - left.gain;
            let tail = next.gain - middle.gain;
            let span = next.sample - left.sample;
            // Merge the nearly settled end of a fast attack into one point.
            // Keep its gain and fit when it settles, using the attack's area
            // instead of preserving two nearly coincident samples.
            if index + 1 < anchors.len() - 1
                && !protected(next.sample)
                && span <= window
                && entry.abs() >= 0.08
                && tail.abs() <= entry.abs() * 0.5
                && entry * tail >= 0.0
            {
                let step = span.div_ceil(8);
                for sample in ((left.sample + step).max(2)..next.sample).step_by(step) {
                    candidate[index].sample = sample;
                    let error = simplification_error(
                        points,
                        &mut candidate,
                        &mut recent,
                        best_error,
                        boundary,
                    );
                    if error < best_error {
                        best_error = error;
                        best = Some(candidate.clone());
                    }
                }
            }
        }
        let Some(simpler) = best else {
            return anchors;
        };
        anchors = simpler;
    }
}

fn simplification_error(
    points: &[f32],
    candidate: &mut [CompressorAnchor],
    recent: &mut [f32],
    limit: f32,
    boundary: FitBoundary,
) -> f32 {
    let window = recent.len();
    let mut sum = 0.0;
    let mut largest = 0.0_f32;
    for segment in 0..candidate.len() - 1 {
        let (tension, cubic) = fit_bezier_segment(points, candidate, segment, boundary);
        candidate[segment].tension = tension;
        let start = candidate[segment].sample;
        let end = candidate[segment + 1].sample;
        for (index, &gain) in points.iter().enumerate().take(end).skip(start) {
            let difference = gain - cubic.sample(index as f32);
            if difference.abs() > SLOW_ENVELOPE_ERROR {
                // Only fast transitions can trade peak error for a better
                // time-averaged fit. Sustained gain stays close pointwise.
                let radius = window / 2;
                let before = points[index.saturating_sub(radius)];
                let after = points[(index + radius).min(points.len() - 1)];
                if (before - after).abs() < 0.2 {
                    return f32::INFINITY;
                }
            }
            let slot = &mut recent[index % window];
            sum += difference;
            if index >= window {
                sum -= *slot;
            }
            *slot = difference;
            largest = largest.max(sum.abs() / window as f32);
        }
        if largest >= limit {
            return largest;
        }
    }
    largest
}

/// Fit one captured cycle to editable cubic Bézier segments off the audio thread.
pub(crate) fn editable_curve_from_capture(points: &[f32]) -> crate::curve::EditableCurve {
    use crate::curve::{CurveNode, CurveSegment, EditableCurve};

    let clipped_seam = (points[0] - points[CAPTURE_POINTS - 1]).abs() > BEZIER_FIT_ERROR;
    let boundary = if clipped_seam {
        FitBoundary::Clipped
    } else {
        FitBoundary::Joined
    };
    let (mut nodes, mut segments): (Vec<_>, Vec<_>) = fit_bezier(points, boundary)
        .into_iter()
        .map(|anchor| {
            let index = anchor.sample;
            (
                CurveNode {
                    x: index as f32 / (CAPTURE_POINTS - 1) as f32,
                    y: anchor.gain.clamp(0.0, 1.0),
                },
                CurveSegment {
                    tension: anchor.tension,
                },
            )
        })
        .unzip();
    segments.pop();
    if clipped_seam {
        // Keep both sides of a sharp cycle cut as nearby authored points.
        // Pump's clip seam preserves interior points at the edge and couples
        // its structural endpoints; a fast closure keeps the transient at
        // the boundary instead of smearing it across the end of the cycle.
        nodes.insert(
            1,
            CurveNode {
                x: CLIPPED_START_PHASE,
                y: points[0].clamp(0.0, 1.0),
            },
        );
        nodes.insert(
            nodes.len() - 1,
            CurveNode {
                x: (CAPTURE_POINTS - 2) as f32 / (CAPTURE_POINTS - 1) as f32,
                y: points[CAPTURE_POINTS - 1].clamp(0.0, 1.0),
            },
        );
        let closure = CurveSegment { tension: -1.0 };
        segments.insert(0, closure);
        segments.push(closure);
    }
    EditableCurve {
        segments,
        nodes,
        origin_is_clip: clipped_seam,
        high_resolution: true,
        smooth_bezier: true,
        ..EditableCurve::default()
    }
    .normalized()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_waits_for_playback_and_restarts_after_seek() {
        use toybox::dsp::TransportState;
        let exchange = CaptureExchange::default();
        exchange.request();
        let mut runner = CaptureRunner::default();
        let params = crate::params::PumpParams::new();
        params.set_compressor_control(0, 1.0);
        let settings = crate::sample_automation::dsp_settings_from_params(&params);
        let stopped = TransportState {
            is_playing: false,
            song_pos_beats: Some(0.0),
            tempo_bpm: 60.0,
        };
        runner.begin_block(&exchange, settings, stopped, 100, 100.0);
        for i in 0..100 {
            runner.record(&exchange, settings, i as f32 / 100.0, 0.5);
        }
        assert!(exchange.snapshot().is_none());
        let playing = TransportState {
            is_playing: true,
            ..stopped
        };
        runner.begin_block(&exchange, settings, playing, 100, 100.0);
        for i in 0..100 {
            runner.record(&exchange, settings, i as f32 / 100.0, 0.5);
        }
        runner.begin_block(
            &exchange,
            settings,
            TransportState {
                song_pos_beats: Some(5.0),
                ..playing
            },
            100,
            100.0,
        );
        for i in 0..100 {
            runner.record(&exchange, settings, i as f32 / 100.0, 0.5);
        }
        assert!(exchange.snapshot().is_none());
        runner.begin_block(
            &exchange,
            settings,
            TransportState {
                song_pos_beats: Some(6.0),
                ..playing
            },
            100,
            100.0,
        );
        for i in 0..100 {
            runner.record(&exchange, settings, i as f32 / 100.0, 0.5);
        }
        assert!(exchange.snapshot().is_none());
        runner.begin_block(
            &exchange,
            settings,
            TransportState {
                song_pos_beats: Some(7.0),
                ..playing
            },
            1,
            100.0,
        );
        runner.record(&exchange, settings, 0.0, 0.5);
        assert!(exchange.snapshot().is_some());
    }

    #[test]
    fn captures_one_complete_eight_bar_cycle() {
        use toybox::dsp::TransportState;
        let exchange = CaptureExchange::default();
        exchange.request();
        let mut runner = CaptureRunner::default();
        let params = crate::params::PumpParams::new();
        params.set_compressor_control(0, 1.0);
        params.set_sync_division(9.0);
        let settings = crate::sample_automation::dsp_settings_from_params(&params);
        for beat in 0..=64 {
            runner.begin_block(
                &exchange,
                settings,
                TransportState {
                    is_playing: true,
                    song_pos_beats: Some(beat as f64),
                    tempo_bpm: 60.0,
                },
                100,
                100.0,
            );
            for sample in 0..100 {
                let phase = ((beat * 100 + sample) % 3200) as f32 / 3200.0;
                runner.record(&exchange, settings, phase, 0.25 + 0.5 * phase);
            }
            if beat < 64 {
                assert!(exchange.snapshot().is_none());
            }
        }
        let points = exchange.snapshot().expect("complete eight-bar capture");
        assert!((points[CAPTURE_POINTS / 2] - 0.5).abs() < 0.001);
    }

    #[test]
    fn exchange_rejects_stale_and_cancelled_takes() {
        let exchange = CaptureExchange::default();
        let first = exchange.request();
        let second = exchange.request();
        assert!(!exchange.publish(first, &[0.2; CAPTURE_POINTS]));
        assert!(exchange.snapshot().is_none());
        assert!(exchange.publish(second, &[0.4; CAPTURE_POINTS]));
        assert!(exchange.snapshot().unwrap().iter().all(|x| *x == 0.4));
        exchange.cancel();
        assert!(exchange.snapshot().is_none());
        assert!(!exchange.publish(second, &[0.3; CAPTURE_POINTS]));
    }

    #[test]
    fn exchange_never_returns_a_torn_snapshot() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let exchange = CaptureExchange::default();
        let request = exchange.request();
        let finished = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for i in 0..200 {
                    let value = if i % 2 == 0 { 0.2 } else { 0.8 };
                    assert!(exchange.publish(request, &[value; CAPTURE_POINTS]));
                }
                finished.store(true, Ordering::Release);
            });
            while !finished.load(Ordering::Acquire) {
                if let Some(points) = exchange.snapshot() {
                    assert!(points.iter().all(|point| *point == points[0]));
                }
            }
        });
    }

    #[test]
    fn exchange_publishes_without_audio_thread_allocations() {
        let exchange = CaptureExchange::default();
        let request = exchange.request();
        crate::test_alloc::assert_no_alloc(|| {
            assert!(exchange.publish(request, &[0.5; CAPTURE_POINTS]));
        });
        assert!(exchange.snapshot().is_some());
    }

    #[test]
    fn fitted_curve_preserves_short_transients_and_stays_editable() {
        let mut points = [1.0; CAPTURE_POINTS];
        points[31] = 0.8;
        points[32] = 0.2;
        points[33] = 0.7;
        let curve = editable_curve_from_capture(&points);
        assert!(curve.nodes.len() <= MAX_COMPRESSOR_ANCHORS);
        for (index, value) in points.iter().enumerate().take(CAPTURE_POINTS - 1) {
            let phase = index as f32 / (CAPTURE_POINTS - 1) as f32;
            let actual = crate::curve::sample_editable_curve(&curve, phase);
            assert!(
                (actual - value).abs() <= 0.0051,
                "point {index}: {actual} vs {value}"
            );
        }
    }

    #[test]
    fn bezier_fit_simplifies_ripple_and_keeps_smooth_bounded_joins() {
        use crate::curve::sample_editable_curve;
        let points: [f32; CAPTURE_POINTS] = std::array::from_fn(|index| {
            let phase = index as f32 / (CAPTURE_POINTS - 1) as f32;
            let envelope = 1.0 - 0.6 * (std::f32::consts::PI * phase).sin().powi(2);
            let ripple = if index % 2 == 0 { 0.001 } else { -0.001 };
            (envelope + ripple).min(1.0)
        });
        let curve = editable_curve_from_capture(&points);
        assert!(curve.smooth_bezier);
        assert!(curve.nodes.len() <= MAX_COMPRESSOR_ANCHORS);
        for (index, point) in points.iter().enumerate().step_by(16) {
            let phase = index as f32 / (CAPTURE_POINTS - 1) as f32;
            assert!(
                (sample_editable_curve(&curve, phase) - point).abs() < SLOW_ENVELOPE_ERROR + 0.0001
            );
        }
        for node in &curve.nodes[..curve.nodes.len() - 1] {
            let dx = 1.0e-4;
            let left = (node.y - sample_editable_curve(&curve, node.x - dx)) / dx;
            let right = (sample_editable_curve(&curve, node.x + dx) - node.y) / dx;
            assert!(
                (left - right).abs() < 0.025,
                "phase {}: {left} vs {right}",
                node.x
            );
        }
    }

    #[test]
    fn compact_duck_curve_keeps_the_attack_and_release() {
        use crate::curve::sample_editable_curve;

        let envelope = |phase: f32| {
            let attack = 0.28 + 0.72 * (-115.0 * phase).exp();
            let release = ((phase - 0.16) / 0.84).max(0.0);
            let release = 0.28 + 0.72 * release * release * (3.0 - 2.0 * release);
            attack.max(release)
        };
        let points: [f32; CAPTURE_POINTS] = std::array::from_fn(|index| {
            let phase = index as f32 / (CAPTURE_POINTS - 1) as f32;
            let ripple = 0.007 * (2200.0 * phase).sin() * (std::f32::consts::PI * phase).sin();
            (envelope(phase) + ripple).clamp(0.0, 1.0)
        });

        let curve = editable_curve_from_capture(&points);
        assert!(curve.smooth_bezier);
        assert!(curve.nodes.len() <= MAX_COMPRESSOR_ANCHORS);
        for index in 0..1024 {
            let phase = index as f32 / 1024.0;
            let gain = sample_editable_curve(&curve, phase);
            assert!(
                (gain - envelope(phase)).abs() < 0.03,
                "phase {phase}, gain {gain}"
            );
        }
    }

    #[test]
    fn steep_attack_does_not_stack_editable_points() {
        use crate::curve::sample_editable_curve;

        for end_gain in [1.0, 0.94] {
            let envelope = |phase: f32| {
                let attack = 0.27 + 0.73 * (-1800.0 * phase).exp();
                let release = 0.27 + (end_gain - 0.27) * phase * phase * (3.0 - 2.0 * phase);
                attack.max(release)
            };
            let points: [f32; CAPTURE_POINTS] = std::array::from_fn(|index| {
                let phase = index as f32 / (CAPTURE_POINTS - 1) as f32;
                envelope(phase)
            });
            let curve = editable_curve_from_capture(&points);
            let nodes = if curve.origin_is_clip {
                &curve.nodes[1..curve.nodes.len() - 1]
            } else {
                curve.nodes.as_slice()
            };
            assert!(nodes.len() <= 5, "points: {nodes:?}");
            let attack_points = nodes.iter().filter(|node| node.x < 0.01).count();
            assert!(attack_points <= 2, "attack points: {nodes:?}");
            assert!((sample_editable_curve(&curve, 0.005) - 0.27).abs() < 0.03);
            for index in 1..100 {
                let phase = index as f32 / 100.0;
                let fitted = sample_editable_curve(&curve, phase);
                let source = envelope(phase);
                assert!(
                    (fitted - source).abs() < 0.03,
                    "end gain {end_gain}, phase {phase}: {fitted} vs {source}, points {nodes:?}"
                );
            }
        }
    }

    #[test]
    fn fitted_curve_keeps_a_sharp_transient_at_the_cycle_cut() {
        let mut points = [1.0; CAPTURE_POINTS];
        points[..100].fill(0.2);
        let curve = editable_curve_from_capture(&points);
        assert!(curve.origin_is_clip);
        assert!((crate::curve::sample_editable_curve(&curve, 0.0) - 0.2).abs() < 0.03);
        assert!((crate::curve::sample_editable_curve(&curve, 0.999) - 1.0).abs() < 0.03);
    }

    #[test]
    fn fitted_constant_curve_needs_only_endpoints() {
        let curve = editable_curve_from_capture(&[0.5; CAPTURE_POINTS]);
        assert_eq!(curve.nodes.len(), 2);
        assert_eq!(curve.nodes[0].y, 0.5);
    }

    #[test]
    fn waits_for_a_full_cycle_and_interpolates_samples() {
        let mut capture = CompressorCapture::new();
        for index in 0..101 {
            let phase = (index % 100) as f32 / 100.0;
            capture.record(phase, phase);
        }
        assert!(capture.snapshot().is_none());
        for index in 1..=100 {
            let phase = (index % 100) as f32 / 100.0;
            capture.record(phase, phase);
        }
        let points = capture.snapshot().unwrap();
        assert!((points[(CAPTURE_POINTS - 1) / 4] - 0.25).abs() < 1e-5);
        assert!((points[(CAPTURE_POINTS - 1) / 2] - 0.5).abs() < 1e-5);
        assert!((points[3 * (CAPTURE_POINTS - 1) / 4] - 0.75).abs() < 1e-5);
        assert_eq!(points[0], 0.0);
        assert!((points[CAPTURE_POINTS - 1] - 0.99).abs() < 1e-5);
    }

    #[test]
    fn discontinuity_discards_partial_capture_and_rearms() {
        let mut capture = CompressorCapture::new();
        for i in 0..=150 {
            capture.record((i % 100) as f32 / 100.0, 0.5);
        }
        capture.record(0.1, 0.5);
        for i in 11..=200 {
            capture.record((i % 100) as f32 / 100.0, 0.5);
        }
        assert!(capture.snapshot().is_some());
        assert!(capture
            .snapshot()
            .unwrap()
            .iter()
            .all(|v| (*v - 0.5).abs() < 1e-6));
        capture.arm();
        assert!(capture.snapshot().is_none());
    }

    #[test]
    fn invalid_values_discard_capture() {
        let mut capture = CompressorCapture::new();
        capture.record(0.1, 1.0);
        capture.record(f32::NAN, 1.0);
        assert!(capture.snapshot().is_none());
        assert!(capture.previous.is_none());
    }
}
