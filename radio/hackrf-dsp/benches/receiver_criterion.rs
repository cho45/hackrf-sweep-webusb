use std::hint::black_box;
use std::time::Duration;

use criterion::{
    criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode, Throughput,
};

use hackrf_dsp::Receiver;

const IQ_BYTES_PER_BLOCK: usize = 262_144;
const IQ_SAMPLES_PER_BLOCK: usize = IQ_BYTES_PER_BLOCK / 2;
const FFT_SIZE: usize = 8192;

fn generate_iq_block(sample_rate: f32, block_index: usize) -> Vec<i8> {
    let mut out = vec![0i8; IQ_BYTES_PER_BLOCK];
    let f1 = 12_000.0_f32;
    let f2 = 65_000.0_f32;
    let phi0 = block_index as f32 * IQ_SAMPLES_PER_BLOCK as f32 / sample_rate;
    for i in 0..IQ_SAMPLES_PER_BLOCK {
        let t = phi0 + i as f32 / sample_rate;
        let re = 0.6 * (2.0 * std::f32::consts::PI * f1 * t).cos()
            + 0.25 * (2.0 * std::f32::consts::PI * f2 * t).sin();
        let im = 0.6 * (2.0 * std::f32::consts::PI * f1 * t).sin()
            - 0.25 * (2.0 * std::f32::consts::PI * f2 * t).cos();
        out[i * 2] = (re.clamp(-0.99, 0.99) * 127.0) as i8;
        out[i * 2 + 1] = (im.clamp(-0.99, 0.99) * 127.0) as i8;
    }
    out
}

fn create_receiver(sample_rate: f32, stereo_enabled: bool) -> Receiver {
    let mut rx = Receiver::new(
        sample_rate,
        100_000_000.0,
        100_000_000.0,
        "FM",
        48_000.0,
        FFT_SIZE,
        0,
        FFT_SIZE,
        0.0,
        98_000.0,
        true,
    );
    rx.set_fm_stereo_enabled(stereo_enabled);
    rx
}

fn bench_receiver_fm_process_into(c: &mut Criterion) {
    let mut group = c.benchmark_group("receiver_fm_process_into");
    group.sample_size(50);
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(8));
    group.noise_threshold(0.03);
    group.significance_level(0.01);
    group.confidence_level(0.99);
    group.sampling_mode(SamplingMode::Flat);
    group.throughput(Throughput::Bytes(IQ_BYTES_PER_BLOCK as u64));

    for sample_rate in [10_000_000.0_f32, 20_000_000.0_f32] {
        for (enabled, label) in [(false, "mono"), (true, "stereo")] {
            let mut rx = create_receiver(sample_rate, enabled);
            let mut audio_out = vec![0.0f32; IQ_BYTES_PER_BLOCK];
            let mut fft_out = vec![0.0f32; FFT_SIZE];
            let blocks: Vec<Vec<i8>> = (0..16)
                .map(|block_index| generate_iq_block(sample_rate, block_index))
                .collect();
            let mut block_cursor = 0usize;

            group.bench_with_input(
                BenchmarkId::new(format!("{:.0}Msps", sample_rate / 1_000_000.0), label),
                &sample_rate,
                |b, _| {
                    b.iter(|| {
                        let block = &blocks[block_cursor];
                        block_cursor = (block_cursor + 1) & 15;
                        let produced = rx.process_into(block, &mut audio_out, &mut fft_out);
                        black_box(produced);
                        black_box(audio_out[0]);
                        black_box(fft_out[0]);
                    });
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_receiver_fm_process_into);
criterion_main!(benches);
