use std::hint::black_box;
use std::time::Instant;

use num_complex::Complex;

use crate::dc::DcCanceller;
use crate::demod::{AMDemodulator, FMDemodulator, Nco};
use crate::fft::FFT;
use crate::filter::DecimationFilter;
use crate::resample::Resampler;
use crate::{
    build_decimation_plan, compute_fir_taps, sanitize_if_band, DemodMode, FIXED_DC_NOTCH_Q,
    FM_MAX_DEVIATION_HZ,
};

const IQ_BYTES_PER_BLOCK: usize = 262_144;
const IQ_SAMPLES_PER_BLOCK: usize = IQ_BYTES_PER_BLOCK / 2;
const FFT_SIZE: usize = 8192;
const AUDIO_SAMPLE_RATE: u32 = 48_000;
const WARMUP_BLOCKS: usize = 8;
const MEASURE_BLOCKS: usize = 60;

#[derive(Clone, Copy)]
struct BenchCase {
    mode: DemodMode,
    sample_rate: f32,
    dc_cancel_enabled: bool,
}

#[derive(Default)]
struct StageStats {
    front_ns: u128,
    front_unpack_ns: u128,
    front_dc_ns: u128,
    front_mix_ns: u128,
    front_fft_pack_ns: u128,
    coarse_ns: u128,
    demod_decim_ns: u128,
    demod_ns: u128,
    resample_ns: u128,
    fft_ns: u128,
    total_ns: u128,
}

impl StageStats {
    fn add_front(&mut self, ns: u128) {
        self.front_ns += ns;
    }
    fn add_front_unpack(&mut self, ns: u128) {
        self.front_unpack_ns += ns;
    }
    fn add_front_dc(&mut self, ns: u128) {
        self.front_dc_ns += ns;
    }
    fn add_front_mix(&mut self, ns: u128) {
        self.front_mix_ns += ns;
    }
    fn add_front_fft_pack(&mut self, ns: u128) {
        self.front_fft_pack_ns += ns;
    }
    fn add_coarse(&mut self, ns: u128) {
        self.coarse_ns += ns;
    }
    fn add_demod_decim(&mut self, ns: u128) {
        self.demod_decim_ns += ns;
    }
    fn add_demod(&mut self, ns: u128) {
        self.demod_ns += ns;
    }
    fn add_resample(&mut self, ns: u128) {
        self.resample_ns += ns;
    }
    fn add_fft(&mut self, ns: u128) {
        self.fft_ns += ns;
    }
    fn add_total(&mut self, ns: u128) {
        self.total_ns += ns;
    }
}

fn ns_to_ms(ns: u128) -> f64 {
    ns as f64 / 1_000_000.0
}

fn print_stage(name: &str, stage_ns: u128, total_ns: u128, blocks: usize) {
    let avg_ms = ns_to_ms(stage_ns) / blocks as f64;
    let ratio = if total_ns > 0 {
        stage_ns as f64 * 100.0 / total_ns as f64
    } else {
        0.0
    };
    println!("  {:>11}: {:>7.3} ms ({:>5.1}%)", name, avg_ms, ratio);
}

fn print_front_stage(name: &str, stage_ns: u128, probe_total_ns: u128, blocks: usize) {
    let avg_ms = ns_to_ms(stage_ns) / blocks as f64;
    let ratio = if probe_total_ns > 0 {
        stage_ns as f64 * 100.0 / probe_total_ns as f64
    } else {
        0.0
    };
    println!("  {:>11}: {:>7.3} ms ({:>5.1}% of probe)", name, avg_ms, ratio);
}

struct FrontProfiler {
    dc_cancel_enabled: bool,
    dc: DcCanceller,
    nco: Nco,
    unpacked: Vec<Complex<f32>>,
    dc_out: Vec<Complex<f32>>,
    mixed: Vec<Complex<f32>>,
    fft_i8: Vec<i8>,
}

impl FrontProfiler {
    fn new(sample_rate: f32, dc_cancel_enabled: bool) -> Self {
        Self {
            dc_cancel_enabled,
            dc: DcCanceller::new(sample_rate, FIXED_DC_NOTCH_Q),
            nco: Nco::new(-250_000.0, sample_rate),
            unpacked: Vec::with_capacity(IQ_SAMPLES_PER_BLOCK),
            dc_out: Vec::with_capacity(IQ_SAMPLES_PER_BLOCK),
            mixed: Vec::with_capacity(IQ_SAMPLES_PER_BLOCK),
            fft_i8: vec![0i8; FFT_SIZE * 2],
        }
    }

    fn profile_block(&mut self, iq: &[i8], stats: &mut StageStats) {
        let t0 = Instant::now();
        self.unpacked.clear();
        for s in iq.chunks_exact(2) {
            self.unpacked
                .push(Complex::new(s[0] as f32 / 128.0, s[1] as f32 / 128.0));
        }
        let unpack_ns = t0.elapsed().as_nanos();

        let (dc_ns, mix_input): (u128, &[Complex<f32>]) = if self.dc_cancel_enabled {
            let t1 = Instant::now();
            self.dc_out.clear();
            for &sample in &self.unpacked {
                self.dc_out.push(self.dc.process(sample));
            }
            (t1.elapsed().as_nanos(), &self.dc_out)
        } else {
            (0, &self.unpacked)
        };

        let t2 = Instant::now();
        self.mixed.clear();
        for &sample in mix_input {
            self.mixed.push(sample * self.nco.step());
        }
        let mix_ns = t2.elapsed().as_nanos();

        let t3 = Instant::now();
        let pack_len = FFT_SIZE.min(self.mixed.len());
        for (idx, mixed) in self.mixed.iter().take(pack_len).enumerate() {
            self.fft_i8[idx * 2] = (mixed.re.clamp(-0.99, 0.99) * 127.0) as i8;
            self.fft_i8[idx * 2 + 1] = (mixed.im.clamp(-0.99, 0.99) * 127.0) as i8;
        }
        let fft_pack_ns = t3.elapsed().as_nanos();

        // 計測ループが最適化で消されないようにする。
        black_box(self.fft_i8[0]);
        black_box(self.mixed.len());

        stats.add_front_unpack(unpack_ns);
        stats.add_front_dc(dc_ns);
        stats.add_front_mix(mix_ns);
        stats.add_front_fft_pack(fft_pack_ns);
    }
}

fn make_window(n: usize) -> Vec<f32> {
    if n == 1 {
        return vec![1.0];
    }
    let mut window = vec![0.0f32; n];
    for (i, w) in window.iter_mut().enumerate() {
        *w = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (n - 1) as f32).cos());
    }
    window
}

fn generate_iq_block(sample_rate: f32, block_index: usize, mode: DemodMode) -> Vec<i8> {
    let mut out = vec![0i8; IQ_SAMPLES_PER_BLOCK * 2];
    let f1 = 12_000.0_f32;
    let f2 = match mode {
        DemodMode::Am => 3_000.0_f32,
        DemodMode::Fm => 65_000.0_f32,
    };
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

fn run_case(case: BenchCase) {
    let plan = build_decimation_plan(case.sample_rate, case.mode);
    let (if_min_hz, if_max_hz) = match case.mode {
        DemodMode::Am => (0.0, 4_500.0),
        DemodMode::Fm => (0.0, 75_000.0),
    };
    let (if_min_hz, if_max_hz) = sanitize_if_band(if_min_hz, if_max_hz, plan.demod_sample_rate);
    let demod_taps = compute_fir_taps(plan.demod_factor);

    let mut nco = Nco::new(-250_000.0, case.sample_rate);
    let mut dc = DcCanceller::new(case.sample_rate, FIXED_DC_NOTCH_Q);
    let mut coarse_filter = DecimationFilter::new_boxcar(plan.coarse_factor);
    let mut demod_filter = DecimationFilter::new_fir_band(
        plan.demod_factor,
        demod_taps,
        if_min_hz / plan.coarse_stage_rate,
        if_max_hz / plan.coarse_stage_rate,
    );
    let mut am = AMDemodulator::new();
    let mut fm = FMDemodulator::new(FM_MAX_DEVIATION_HZ, plan.demod_sample_rate);
    let mut resampler = Resampler::new(plan.demod_sample_rate.round() as u32, AUDIO_SAMPLE_RATE);
    let window = make_window(FFT_SIZE);
    let mut fft = FFT::new(FFT_SIZE, &window);

    let mut baseband = Vec::<Complex<f32>>::with_capacity(IQ_SAMPLES_PER_BLOCK);
    let mut coarse_buf = Vec::<Complex<f32>>::with_capacity(IQ_SAMPLES_PER_BLOCK);
    let mut demod_iq_buf = Vec::<Complex<f32>>::with_capacity(IQ_SAMPLES_PER_BLOCK);
    let mut demod_buf = Vec::<f32>::new();
    let mut audio_buf = Vec::<f32>::new();
    let mut fft_i8 = vec![0i8; FFT_SIZE * 2];
    let mut fft_out = vec![0f32; FFT_SIZE];
    let mut stats = StageStats::default();
    let total_blocks = WARMUP_BLOCKS + MEASURE_BLOCKS;

    for block in 0..total_blocks {
        let iq = generate_iq_block(case.sample_rate, block, case.mode);
        let total_start = Instant::now();

        let t0 = Instant::now();
        baseband.clear();
        if case.dc_cancel_enabled {
            for (idx, s) in iq.chunks_exact(2).enumerate() {
                let raw = Complex::new(s[0] as f32 / 128.0, s[1] as f32 / 128.0);
                let sample = dc.process(raw);
                let mixed = sample * nco.step();
                baseband.push(mixed);
                if idx < FFT_SIZE {
                    fft_i8[idx * 2] = (mixed.re.clamp(-0.99, 0.99) * 127.0) as i8;
                    fft_i8[idx * 2 + 1] = (mixed.im.clamp(-0.99, 0.99) * 127.0) as i8;
                }
            }
        } else {
            for (idx, s) in iq.chunks_exact(2).enumerate() {
                let sample = Complex::new(s[0] as f32 / 128.0, s[1] as f32 / 128.0);
                let mixed = sample * nco.step();
                baseband.push(mixed);
                if idx < FFT_SIZE {
                    fft_i8[idx * 2] = (mixed.re.clamp(-0.99, 0.99) * 127.0) as i8;
                    fft_i8[idx * 2 + 1] = (mixed.im.clamp(-0.99, 0.99) * 127.0) as i8;
                }
            }
        }
        let front_ns = t0.elapsed().as_nanos();

        let t1 = Instant::now();
        coarse_filter.process_into(&baseband, &mut coarse_buf);
        let coarse_ns = t1.elapsed().as_nanos();

        let t2 = Instant::now();
        demod_filter.process_into(&coarse_buf, &mut demod_iq_buf);
        let demod_decim_ns = t2.elapsed().as_nanos();

        let t3 = Instant::now();
        demod_buf.resize(demod_iq_buf.len(), 0.0);
        match case.mode {
            DemodMode::Am => am.demodulate(&demod_iq_buf, &mut demod_buf),
            DemodMode::Fm => fm.demodulate(&demod_iq_buf, &mut demod_buf),
        }
        let demod_ns = t3.elapsed().as_nanos();

        let t4 = Instant::now();
        audio_buf.clear();
        resampler.process(&demod_buf, &mut audio_buf);
        let resample_ns = t4.elapsed().as_nanos();

        let t5 = Instant::now();
        fft.fft(&fft_i8, &mut fft_out);
        let fft_ns = t5.elapsed().as_nanos();

        let total_ns = total_start.elapsed().as_nanos();

        if block >= WARMUP_BLOCKS {
            stats.add_front(front_ns);
            stats.add_coarse(coarse_ns);
            stats.add_demod_decim(demod_decim_ns);
            stats.add_demod(demod_ns);
            stats.add_resample(resample_ns);
            stats.add_fft(fft_ns);
            stats.add_total(total_ns);
        }
    }

    // 詳細内訳は別パスで計測して、パイプライン全体計測への干渉を避ける。
    let mut front_profiler = FrontProfiler::new(case.sample_rate, case.dc_cancel_enabled);
    for block in 0..total_blocks {
        let iq = generate_iq_block(case.sample_rate, block, case.mode);
        if block >= WARMUP_BLOCKS {
            front_profiler.profile_block(&iq, &mut stats);
        }
    }

    let avg_total_ms = ns_to_ms(stats.total_ns) / MEASURE_BLOCKS as f64;
    let blocks_per_sec = if avg_total_ms > 0.0 {
        1000.0 / avg_total_ms
    } else {
        0.0
    };
    let iq_mb_s = blocks_per_sec * IQ_BYTES_PER_BLOCK as f64 / 1_000_000.0;

    println!(
        "Case: mode={:?} rx={:.1}Msps coarse=/{} demod=/{} demod_sr={:.0}Hz",
        case.mode,
        case.sample_rate / 1_000_000.0,
        plan.coarse_factor,
        plan.demod_factor,
        plan.demod_sample_rate
    );
    println!(
        "  options: dc_cancel={}",
        if case.dc_cancel_enabled { "on" } else { "off" }
    );
    print_stage("front", stats.front_ns, stats.total_ns, MEASURE_BLOCKS);
    let front_probe_total =
        stats.front_unpack_ns + stats.front_dc_ns + stats.front_mix_ns + stats.front_fft_pack_ns;
    print_stage(
        "front_probe",
        front_probe_total,
        stats.total_ns,
        MEASURE_BLOCKS,
    );
    print_front_stage(
        "front_unpack",
        stats.front_unpack_ns,
        front_probe_total,
        MEASURE_BLOCKS,
    );
    print_front_stage(
        "front_dc",
        stats.front_dc_ns,
        front_probe_total,
        MEASURE_BLOCKS,
    );
    print_front_stage(
        "front_mix",
        stats.front_mix_ns,
        front_probe_total,
        MEASURE_BLOCKS,
    );
    print_front_stage(
        "front_pack",
        stats.front_fft_pack_ns,
        front_probe_total,
        MEASURE_BLOCKS,
    );
    print_stage("coarse", stats.coarse_ns, stats.total_ns, MEASURE_BLOCKS);
    print_stage("demod_decim", stats.demod_decim_ns, stats.total_ns, MEASURE_BLOCKS);
    print_stage("demod", stats.demod_ns, stats.total_ns, MEASURE_BLOCKS);
    print_stage("resample", stats.resample_ns, stats.total_ns, MEASURE_BLOCKS);
    print_stage("fft", stats.fft_ns, stats.total_ns, MEASURE_BLOCKS);
    println!(
        "  {:>11}: {:>7.3} ms  blocks/s={:>6.1}  IQ MB/s={:>6.2}",
        "total", avg_total_ms, blocks_per_sec, iq_mb_s
    );
    println!();
}

pub fn run_default_pipeline_bench() {
    println!("hackrf-dsp pipeline benchmark (native)");
    println!(
        "block={} bytes, warmup={} blocks, measure={} blocks",
        IQ_BYTES_PER_BLOCK, WARMUP_BLOCKS, MEASURE_BLOCKS
    );
    println!();

    let cases = [
        BenchCase {
            mode: DemodMode::Am,
            sample_rate: 2_000_000.0,
            dc_cancel_enabled: true,
        },
        BenchCase {
            mode: DemodMode::Am,
            sample_rate: 2_000_000.0,
            dc_cancel_enabled: false,
        },
        BenchCase {
            mode: DemodMode::Am,
            sample_rate: 10_000_000.0,
            dc_cancel_enabled: true,
        },
        BenchCase {
            mode: DemodMode::Am,
            sample_rate: 10_000_000.0,
            dc_cancel_enabled: false,
        },
        BenchCase {
            mode: DemodMode::Am,
            sample_rate: 20_000_000.0,
            dc_cancel_enabled: true,
        },
        BenchCase {
            mode: DemodMode::Am,
            sample_rate: 20_000_000.0,
            dc_cancel_enabled: false,
        },
        BenchCase {
            mode: DemodMode::Fm,
            sample_rate: 2_000_000.0,
            dc_cancel_enabled: true,
        },
        BenchCase {
            mode: DemodMode::Fm,
            sample_rate: 2_000_000.0,
            dc_cancel_enabled: false,
        },
        BenchCase {
            mode: DemodMode::Fm,
            sample_rate: 10_000_000.0,
            dc_cancel_enabled: true,
        },
        BenchCase {
            mode: DemodMode::Fm,
            sample_rate: 10_000_000.0,
            dc_cancel_enabled: false,
        },
        BenchCase {
            mode: DemodMode::Fm,
            sample_rate: 20_000_000.0,
            dc_cancel_enabled: true,
        },
        BenchCase {
            mode: DemodMode::Fm,
            sample_rate: 20_000_000.0,
            dc_cancel_enabled: false,
        },
    ];

    for case in cases {
        run_case(case);
    }
}
