use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct TrackingAllocator;

fn record_allocation() {
    if TRACK.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
    }
}

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        record_allocation();
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_allocation();
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

fn voice(rate: f64, frames: usize, channels: usize) -> Vec<u8> {
    let mut output = Vec::with_capacity(frames * channels * 4);
    for frame in 0..frames {
        let t = frame as f64 / rate;
        let sample = (0.25 * (std::f64::consts::TAU * 230.0 * t).sin()
            + 0.1 * (std::f64::consts::TAU * 1_170.0 * t).sin()) as f32;
        for channel in 0..channels {
            output.extend_from_slice(&(if channel == 0 { sample } else { 0.0_f32 }).to_le_bytes());
        }
    }
    output
}

fn processor(rate: f64, channels: usize) -> AudioProcessor {
    let mut dsp = AudioProcessor::default();
    dsp.configure(
        SampleFormat::Float32,
        channels,
        rate,
        Some(ModelKind::Gtcrn),
    )
    .unwrap();
    dsp
}

#[test]
fn packet_sizes_do_not_change_audio_and_channels_are_independent() {
    for rate in [8_000.0, 16_000.0, 44_100.0, 48_000.0, 192_000.0] {
        let original = voice(rate, (rate * 0.12) as usize, 2);
        let mut whole = original.clone();
        processor(rate, 2).process(&mut whole);
        let mut split = original;
        let mut dsp = processor(rate, 2);
        let blocks = [1, 7, 63, 128, 2, 400, 513];
        let mut position = 0;
        let mut block = 0;
        while position < split.len() {
            let end = (position + blocks[block % blocks.len()] * 8).min(split.len());
            dsp.process(&mut split[position..end]);
            position = end;
            block += 1;
        }
        assert_eq!(whole, split, "packet-size dependence at {rate} Hz");
        let values: Vec<_> = whole
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes))
            .collect();
        assert!(values.iter().all(|value| value.is_finite()));
        assert!(values.iter().step_by(2).any(|value| value.abs() > 0.001));
        assert!(values.iter().skip(1).step_by(2).all(|value| *value == 0.0));
    }
}

#[test]
fn process_and_reset_do_not_allocate_or_free_memory() {
    for rate in [8_000.0, 16_000.0, 44_100.0, 48_000.0, 192_000.0] {
        let mut dsp = processor(rate, 2);
        let input = voice(rate, (rate * 0.2) as usize, 2);
        let mut buffer = input.clone();
        ALLOCATIONS.set(0);
        TRACK.set(true);
        dsp.process(&mut buffer);
        dsp.reset();
        buffer.copy_from_slice(&input);
        dsp.process(&mut buffer);
        TRACK.set(false);
        assert_eq!(ALLOCATIONS.get(), 0, "rate={rate}: real-time heap activity");
    }
}

#[test]
fn reset_clears_model_resampler_and_pending_audio() {
    let mut dsp = processor(44_100.0, 1);
    let input = voice(44_100.0, 8_820, 1);
    let mut first = input.clone();
    dsp.process(&mut first);
    let mut extra = input.clone();
    dsp.process(&mut extra);
    dsp.reset();
    let mut reset = input;
    dsp.process(&mut reset);
    assert_eq!(first, reset);
    dsp.reset();
    let mut silence = vec![0; 8_820 * 4];
    assert!(dsp.process(&mut silence));
    assert!(silence.iter().all(|byte| *byte == 0));
}

#[test]
fn integer_codec_preserves_extremes_valid_bits_and_padding() {
    for bytes in 1..=4 {
        for bits in [bytes * 8, bytes * 8 - 4] {
            let format = SampleFormat::pcm(bytes, bits).unwrap();
            let limit = 1_i64 << (bits - 1);
            let padding = bytes * 8 - bits;
            for value in [-limit, limit - 1, -limit / 2, 0, limit / 2] {
                let mut sample = vec![0; bytes as usize];
                let container = value << padding;
                if bytes == 1 {
                    sample[0] = (container + 128) as u8;
                } else {
                    sample.copy_from_slice(&(container as i32).to_le_bytes()[..bytes as usize]);
                }
                let original = sample.clone();
                assert_eq!(format.decode(&sample), value as f64 / limit as f64);
                format.encode(&mut sample, format.decode(&original));
                assert_eq!(sample, original);
            }
        }
    }
}

#[test]
fn nonfinite_and_overrange_float_samples_are_safe() {
    for format in [SampleFormat::Float32, SampleFormat::Float64] {
        for input in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e30, -1e30] {
            let encoded = if matches!(format, SampleFormat::Float32) {
                (input as f32).to_le_bytes().to_vec()
            } else {
                input.to_le_bytes().to_vec()
            };
            let decoded = format.decode(&encoded);
            assert!(decoded.is_finite() && (-1.0..=1.0).contains(&decoded));
            if !input.is_finite() {
                assert_eq!(decoded, 0.0);
            }
        }
    }
}

#[test]
fn reported_latency_matches_an_identity_model_impulse() {
    struct Identity(spectral::Stft);
    impl StreamingModel for Identity {
        fn sample_rate(&self) -> u32 {
            16_000
        }
        fn hop_size(&self) -> usize {
            256
        }
        fn latency_samples(&self) -> usize {
            256
        }
        fn reset(&mut self) {
            self.0.reset();
        }
        fn process(&mut self, input: &[f32], output: &mut [f32]) {
            let spectrum = self.0.analyze(input);
            self.0.synthesize(&spectrum, output);
        }
    }
    for rate in [
        8_000.0, 11_025.0, 16_000.0, 22_050.0, 24_000.0, 32_000.0, 44_100.0, 48_000.0, 48_001.0,
        96_000.0, 176_400.0, 192_000.0,
    ] {
        let dsp = processor(rate, 1);
        let expected = dsp.latency_hns() as f64 * rate / 10_000_000.0;
        let mut channel = Channel::new(Box::new(Identity(spectral::Stft::new())), rate);
        for impulse in [0, (rate * 0.025) as usize + 7] {
            channel.reset();
            let mut peak = (0, 0.0_f32);
            for i in 0..(rate * 0.12) as usize {
                let value = channel.process_sample(if i == impulse { 0.5 } else { 0.0 });
                if value.abs() > peak.1 {
                    peak = (i, value.abs());
                }
            }
            assert!(peak.1 > 0.02);
            assert!(
                ((peak.0 - impulse) as f64 - expected).abs() < 2.0,
                "rate={rate} impulse={impulse} impulse_peak={} reported={expected}",
                peak.0
            );
        }
    }
}
