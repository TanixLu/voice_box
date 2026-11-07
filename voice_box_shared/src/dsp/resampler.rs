use std::f64::consts::PI;

use rubato::{
    Async, FixedAsync, Resampler as _, SincInterpolationParameters, SincInterpolationType,
    WindowFunction, audioadapter_buffers::direct::SequentialSlice,
};

/// Rubato sinc conversion with fixed one-millisecond input chunks and buffers.
pub struct StreamingResampler {
    engine: Option<Async<f32>>,
    input: Vec<f32>,
    output: Vec<f32>,
    pending: usize,
    read: usize,
    available: usize,
    delay_seconds: f64,
}

impl StreamingResampler {
    pub fn new(input_rate: f64, output_rate: f64) -> Self {
        if input_rate == output_rate {
            return Self {
                engine: None,
                input: vec![0.0],
                output: vec![0.0],
                pending: 0,
                read: 0,
                available: 0,
                delay_seconds: 0.0,
            };
        }
        let ratio = output_rate / input_rate;
        let parameters = SincInterpolationParameters {
            sinc_len: (64.0 / ratio.min(1.0)).ceil() as usize,
            f_cutoff: Some(0.9),
            oversampling_factor: 128,
            interpolation: SincInterpolationType::Linear,
            window: WindowFunction::Blackman,
        };
        let engine = Async::new_sinc(
            ratio,
            1.0,
            &parameters,
            (input_rate / 1000.0).ceil() as usize,
            1,
            FixedAsync::Input,
        )
        .expect("UNREACHABLE: validated rates and fixed parameters should create a resampler");
        // Account for Rubato's sinc-table phase offset and initial index advance.
        let sinc_len = parameters.sinc_len.next_multiple_of(8);
        let delay_seconds = (sinc_len as f64 / 2.0 - 1.0 / parameters.oversampling_factor as f64)
            / input_rate
            - 1.0 / output_rate;
        Self {
            input: vec![0.0; engine.input_frames_max()],
            output: vec![0.0; engine.output_frames_max()],
            engine: Some(engine),
            pending: 0,
            read: 0,
            available: 0,
            delay_seconds,
        }
    }

    pub fn delay_seconds(&self) -> f64 {
        self.delay_seconds
    }
    pub fn input_chunk_size(&self) -> usize {
        if self.engine.is_some() {
            self.input.len()
        } else {
            0
        }
    }

    pub fn reset(&mut self) {
        if let Some(engine) = &mut self.engine {
            engine.reset();
        }
        self.input.fill(0.0);
        self.output.fill(0.0);
        self.pending = 0;
        self.read = 0;
        self.available = 0;
    }

    pub fn push(&mut self, sample: f32) {
        self.input[self.pending] = sample;
        self.pending += 1;
        if self.pending != self.input.len() {
            return;
        }
        assert_eq!(
            self.read, self.available,
            "previous resampler output must be drained"
        );
        self.pending = 0;
        self.read = 0;
        self.available = if let Some(engine) = &mut self.engine {
            let input = SequentialSlice::new(&self.input, 1, self.input.len())
                .expect("UNREACHABLE: mono input view should fit its backing slice");
            let output_len = self.output.len();
            let mut output = SequentialSlice::new_mut(&mut self.output, 1, output_len)
                .expect("UNREACHABLE: mono output view should fit its backing slice");
            let (consumed, produced) = engine
                .process_into_buffer(&input, &mut output, None)
                .expect("UNREACHABLE: preallocated buffers should satisfy resampler requirements");
            debug_assert_eq!(consumed, self.input.len());
            produced
        } else {
            self.output[0] = sample;
            1
        };
    }

    pub fn next_sample(&mut self) -> Option<f32> {
        if self.read == self.available {
            return None;
        }
        let sample = self.output[self.read];
        self.read += 1;
        Some(sample)
    }
}
const TAPS: usize = 32;
const PHASES: usize = 256;

/// Windowed-sinc resampling at caller-selected source positions.
/// Owns only immutable filter coefficients, without a source cursor or history.
/// Construction allocates; sampling never allocates or changes the coefficients.
pub struct RandomAccessResampler {
    kernels: Box<[[f32; TAPS]]>,
    source_step: f64,
}

impl RandomAccessResampler {
    /// Rates must be finite and positive.
    pub fn new(input_rate: f64, output_rate: f64) -> Self {
        debug_assert!(input_rate.is_finite() && input_rate > 0.0);
        debug_assert!(output_rate.is_finite() && output_rate > 0.0);
        let cutoff = (output_rate / input_rate).min(1.0) * 0.90;
        let kernels = (0..PHASES)
            .map(|phase| {
                let mut row = [0.0; TAPS];
                let mut sum = 0.0;
                for (i, value) in row.iter_mut().enumerate() {
                    let x = i as f64 - (TAPS / 2 - 1) as f64 - phase as f64 / PHASES as f64;
                    let sinc = if x.abs() < 1e-10 {
                        cutoff
                    } else {
                        (PI * cutoff * x).sin() / (PI * x)
                    };
                    let window = 0.42 - 0.5 * (2.0 * PI * i as f64 / (TAPS - 1) as f64).cos()
                        + 0.08 * (4.0 * PI * i as f64 / (TAPS - 1) as f64).cos();
                    *value = (sinc * window) as f32;
                    sum += *value;
                }
                for value in &mut row {
                    *value /= sum;
                }
                row
            })
            .collect();
        Self {
            kernels,
            source_step: input_rate / output_rate,
        }
    }

    /// Nominal source-frame advance per output frame; the caller owns the cursor.
    pub fn source_step(&self) -> f64 {
        self.source_step
    }

    /// Read a frame at a finite, nonnegative source position.
    /// The callback owns storage lookup and supplies silence for missing frames.
    /// Filter indices before the start of the source are clamped to zero.
    pub fn sample<const CHANNELS: usize>(
        &self,
        position: f64,
        mut read: impl FnMut(u64) -> [f32; CHANNELS],
    ) -> [f32; CHANNELS] {
        debug_assert!(position.is_finite() && position >= 0.0);
        let base = position.floor() as u64;
        let fraction = position.fract();
        if self.source_step == 1.0 && fraction == 0.0 {
            return read(base);
        }
        let phase = ((fraction * PHASES as f64) as usize).min(PHASES - 1);
        let mut output = [0.0; CHANNELS];
        for (i, &coefficient) in self.kernels[phase].iter().enumerate() {
            let frame = base
                .saturating_add(i as u64)
                .saturating_sub((TAPS / 2 - 1) as u64);
            for (value, sample) in output.iter_mut().zip(read(frame)) {
                *value += sample * coefficient;
            }
        }
        output
    }
}
