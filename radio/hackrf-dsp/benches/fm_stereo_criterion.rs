use std::hint::black_box;
use std::time::Duration;

use criterion::{
    criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode, Throughput,
};

use hackrf_dsp::demod::FMStereoDecoder;

// 10Msps, block=262_144 bytes のとき FM demod 側は約 2_621 サンプル/ブロック。
const BLOCK_SAMPLES: usize = 2_622;

#[derive(Clone, Copy, Debug)]
enum FMStereoInputKind {
    StereoProgram,
    MonoWithPilot,
}

fn build_fm_stereo_mpx_block(
    sample_rate_hz: f32,
    block_index: usize,
    samples: usize,
    kind: FMStereoInputKind,
) -> Vec<f32> {
    let mut out = Vec::with_capacity(samples);
    let t0 = block_index as f32 * samples as f32 / sample_rate_hz;
    for i in 0..samples {
        let t = t0 + i as f32 / sample_rate_hz;
        let pilot = 0.10 * (2.0 * std::f32::consts::PI * 19_000.0 * t).cos();
        let mono = 0.28 * (2.0 * std::f32::consts::PI * 1_200.0 * t).sin()
            + 0.20 * (2.0 * std::f32::consts::PI * 2_200.0 * t).sin();
        let lr = match kind {
            FMStereoInputKind::StereoProgram => {
                0.26 * (2.0 * std::f32::consts::PI * 700.0 * t).sin()
                    + 0.17 * (2.0 * std::f32::consts::PI * 2_700.0 * t).sin()
            }
            FMStereoInputKind::MonoWithPilot => 0.0,
        };
        let dsb = lr * (2.0 * std::f32::consts::PI * 38_000.0 * t).cos();
        out.push(mono + pilot + dsb);
    }
    out
}

struct FMStereoProcessFixture {
    decoder: FMStereoDecoder,
    mpx: Vec<f32>,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl FMStereoProcessFixture {
    fn new(
        sample_rate_hz: f32,
        samples_per_block: usize,
        kind: FMStereoInputKind,
        block_index: usize,
    ) -> Self {
        Self {
            decoder: FMStereoDecoder::new(sample_rate_hz, Some(50.0)),
            mpx: build_fm_stereo_mpx_block(sample_rate_hz, block_index, samples_per_block, kind),
            left: Vec::with_capacity(samples_per_block),
            right: Vec::with_capacity(samples_per_block),
        }
    }

    fn process_once(&mut self) -> usize {
        self.decoder
            .process(&self.mpx, &mut self.left, &mut self.right);
        self.left.len().min(self.right.len())
    }

    fn output_probe(&self) -> (f32, f32) {
        (
            self.left.first().copied().unwrap_or(0.0),
            self.right.first().copied().unwrap_or(0.0),
        )
    }
}

fn bench_fm_stereo_process(c: &mut Criterion) {
    let mut group = c.benchmark_group("fm_stereo_process");
    group.sample_size(50);
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(8));
    group.noise_threshold(0.03);
    group.significance_level(0.01);
    group.confidence_level(0.99);
    group.sampling_mode(SamplingMode::Flat);

    for sample_rate_hz in [200_000.0f32] {
        for (kind, label) in [
            (FMStereoInputKind::StereoProgram, "stereo_program"),
            (FMStereoInputKind::MonoWithPilot, "mono_with_pilot"),
        ] {
            let mut fixtures: Vec<FMStereoProcessFixture> = (0..16)
                .map(|block_idx| FMStereoProcessFixture::new(sample_rate_hz, BLOCK_SAMPLES, kind, block_idx))
                .collect();
            let mut cursor = 0usize;
            group.throughput(Throughput::Elements(BLOCK_SAMPLES as u64));
            group.bench_with_input(
                BenchmarkId::new(format!("{:.0}kHz", sample_rate_hz / 1_000.0), label),
                &sample_rate_hz,
                |b, _| {
                    b.iter(|| {
                        let fixture = &mut fixtures[cursor];
                        cursor = (cursor + 1) & 15;
                        let produced = fixture.process_once();
                        let (l0, r0) = fixture.output_probe();
                        black_box(produced);
                        black_box(l0);
                        black_box(r0);
                    });
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, bench_fm_stereo_process);
criterion_main!(benches);
