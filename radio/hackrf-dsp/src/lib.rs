#![deny(warnings)]
#![deny(clippy::all)]

mod dc;
mod demod;
mod fft;
mod filter;
mod resample;
#[cfg(not(target_arch = "wasm32"))]
pub mod bench;

use num_complex::Complex;
use wasm_bindgen::prelude::*;

use crate::dc::DcCanceller;
use crate::demod::{AMDemodulator, FMDemodulator, Nco};
use crate::fft::FFT;
use crate::filter::DecimationFilter;
use crate::resample::Resampler;

// 固定QのDCノッチ。2MHz時に等価ノッチ幅は約2kHz。
const FIXED_DC_NOTCH_Q: f32 = 1_000.0;

/// FM の最大周波数偏移 [Hz]。WFM（ワイドFM放送）想定。
const FM_MAX_DEVIATION_HZ: f32 = 75_000.0;

/// モード別の復調レート [Hz]
const AM_DEMOD_RATE: f32 = 50_000.0;
const FM_DEMOD_RATE: f32 = 200_000.0;
const COARSE_STAGE_RATE: f32 = 1_000_000.0;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

/// 復調モード
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DemodMode {
    Am,
    Fm,
}

impl DemodMode {
    fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "AM" => Some(Self::Am),
            "FM" => Some(Self::Fm),
            _ => None,
        }
    }

    fn demod_rate(self) -> f32 {
        match self {
            Self::Am => AM_DEMOD_RATE,
            Self::Fm => FM_DEMOD_RATE,
        }
    }
}

/// デシメーション用 FIR タップ数を factor から算出する。
fn compute_fir_taps(factor: usize) -> usize {
    let raw = (factor * 15).max(31).min(1001);
    raw | 1 // 奇数保証
}

struct DecimationPlan {
    target_demod_rate: f32,
    coarse_factor: usize,
    coarse_stage_rate: f32,
    demod_factor: usize,
    demod_sample_rate: f32,
}

fn build_decimation_plan(sample_rate: f32, mode: DemodMode) -> DecimationPlan {
    let target_demod_rate = mode.demod_rate();
    let coarse_factor = (sample_rate / COARSE_STAGE_RATE).round().max(1.0) as usize;
    let coarse_stage_rate = sample_rate / coarse_factor as f32;
    let demod_factor = (coarse_stage_rate / target_demod_rate).round().max(1.0) as usize;
    let demod_sample_rate = coarse_stage_rate / demod_factor as f32;
    DecimationPlan {
        target_demod_rate,
        coarse_factor,
        coarse_stage_rate,
        demod_factor,
        demod_sample_rate,
    }
}

/// JS側から呼び出されるラジオのメインDSPレシーバ
#[wasm_bindgen]
pub struct Receiver {
    sample_rate: f32,
    coarse_stage_rate: f32,
    demod_sample_rate: f32,
    if_min_hz: f32,
    if_max_hz: f32,
    dc_cancel_enabled: bool,
    fft_use_processed: bool,
    fft_visible_start: usize,
    fft_visible_len: usize,
    mode: DemodMode,
    nco: Nco,
    dc_canceller: DcCanceller,
    coarse_filter: DecimationFilter,
    demod_filter: DecimationFilter,
    am_demod: AMDemodulator,
    fm_demod: FMDemodulator,
    resampler: Resampler,
    fft: FFT,

    // 中間バッファ（アロケーションを避けるため保持）
    baseband_buffer: Vec<Complex<f32>>,
    coarse_buffer: Vec<Complex<f32>>,
    demod_iq_buffer: Vec<Complex<f32>>,
    demod_buffer: Vec<f32>,
    audio_buffer: Vec<f32>,
    fft_buffer: Vec<f32>,
    fft_visible_buffer: Vec<f32>,
    fft_input_buffer: Vec<i8>,
}

fn sanitize_if_band(min_hz: f32, max_hz: f32, demod_sample_rate: f32) -> (f32, f32) {
    let max_allowed = (demod_sample_rate * 0.49).max(200.0);
    let mut min = min_hz.max(0.0);
    let mut max = max_hz.max(0.0);

    if min >= max_allowed {
        min = 0.0;
    }
    if max <= min {
        max = min + 100.0;
    }
    max = max.min(max_allowed);
    if max <= min {
        min = 0.0;
        max = max_allowed.min(4_500.0);
    }

    (min, max)
}

fn sanitize_fft_view(fft_size: usize, start_bin: usize, visible_bins: usize) -> (usize, usize) {
    let safe_start = start_bin.min(fft_size.saturating_sub(1));
    let max_len = fft_size - safe_start;
    let safe_len = visible_bins.clamp(1, max_len);
    (safe_start, safe_len)
}

fn float_to_i8(sample: f32) -> i8 {
    let scaled = (sample.clamp(-1.0, 0.992_187_5) * 128.0).round();
    scaled as i8
}

#[wasm_bindgen]
impl Receiver {
    #[wasm_bindgen(constructor)]
    pub fn new(
        sample_rate: f32,
        center_freq: f32,
        target_freq: f32,
        demod_mode: &str,
        output_sample_rate: f32,
        fft_size: usize,
        fft_visible_start_bin: usize,
        fft_visible_bins: usize,
        if_min_hz: f32,
        if_max_hz: f32,
        dc_cancel_enabled: bool,
        fft_use_processed: bool,
    ) -> Self {
        console_error_panic_hook::set_once();

        let mode = DemodMode::from_str(demod_mode).unwrap_or(DemodMode::Am);
        let plan = build_decimation_plan(sample_rate, mode);

        let (if_min_hz, if_max_hz) = sanitize_if_band(if_min_hz, if_max_hz, plan.demod_sample_rate);

        let offset_hz = target_freq - center_freq;

        // 粗段: 1Msps正規化は軽量boxcarデシメーション、固定段はモード別FIR。
        let demod_fir_taps = compute_fir_taps(plan.demod_factor);
        let min_cutoff_norm = if_min_hz / plan.coarse_stage_rate;
        let max_cutoff_norm = if_max_hz / plan.coarse_stage_rate;

        log(&format!(
            "[Receiver::new] mode={:?} sr={} coarse_factor={} coarse_sr={} demod_factor={} demod_sr={} target_demod_rate={} if=[{},{}]",
            mode,
            sample_rate,
            plan.coarse_factor,
            plan.coarse_stage_rate,
            plan.demod_factor,
            plan.demod_sample_rate,
            plan.target_demod_rate,
            if_min_hz,
            if_max_hz
        ));

        let resampler = Resampler::new(
            plan.demod_sample_rate.round() as u32,
            output_sample_rate.round() as u32,
        );

        // FFT窓関数 (Hann窓)
        let mut window = vec![0.0f32; fft_size];
        if fft_size == 1 {
            window[0] = 1.0;
        } else {
            for (i, w) in window.iter_mut().enumerate() {
                *w = 0.5
                    * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (fft_size - 1) as f32).cos());
            }
        }

        let (fft_visible_start, fft_visible_len) =
            sanitize_fft_view(fft_size, fft_visible_start_bin, fft_visible_bins);

        Self {
            sample_rate,
            coarse_stage_rate: plan.coarse_stage_rate,
            demod_sample_rate: plan.demod_sample_rate,
            if_min_hz,
            if_max_hz,
            dc_cancel_enabled,
            fft_use_processed,
            fft_visible_start,
            fft_visible_len,
            mode,
            nco: Nco::new(-offset_hz, sample_rate),
            dc_canceller: DcCanceller::new(sample_rate, FIXED_DC_NOTCH_Q),
            coarse_filter: {
                let f = DecimationFilter::new_boxcar(plan.coarse_factor);
                log(&format!(
                    "[CoarseFilter] type=boxcar factor={} dc_gain={:.6} num_coeffs={}",
                    plan.coarse_factor,
                    f.coeffs_dc_gain(),
                    plan.coarse_factor
                ));
                f
            },
            demod_filter: {
                let f = DecimationFilter::new_fir_band(
                    plan.demod_factor,
                    demod_fir_taps,
                    min_cutoff_norm,
                    max_cutoff_norm,
                );
                log(&format!(
                    "[DemodFilter] factor={} band_norm=[{},{}] dc_gain={:.6} num_coeffs={}",
                    plan.demod_factor,
                    min_cutoff_norm,
                    max_cutoff_norm,
                    f.coeffs_dc_gain(),
                    demod_fir_taps
                ));
                f
            },
            am_demod: AMDemodulator::new(),
            fm_demod: FMDemodulator::new(FM_MAX_DEVIATION_HZ, plan.demod_sample_rate),
            resampler,
            fft: FFT::new(fft_size, &window),
            baseband_buffer: Vec::with_capacity(131_072),
            coarse_buffer: Vec::with_capacity(131_072),
            demod_iq_buffer: Vec::with_capacity(131_072),
            demod_buffer: Vec::with_capacity(8_192),
            audio_buffer: Vec::with_capacity(8_192),
            fft_buffer: vec![0.0; fft_size],
            fft_visible_buffer: vec![-120.0; fft_visible_len],
            fft_input_buffer: vec![0; fft_size * 2],
        }
    }

    /// 受信対象の周波数（あるいはオフセット）を変更する
    pub fn set_target_freq(&mut self, center_freq: f32, target_freq: f32) {
        let offset_hz = target_freq - center_freq;
        self.nco = Nco::new(-offset_hz, self.sample_rate);
    }

    /// IFチャンネルフィルタの通過帯域を変更する（Hz）
    pub fn set_if_band(&mut self, min_hz: f32, max_hz: f32) {
        let (min_hz, max_hz) = sanitize_if_band(min_hz, max_hz, self.demod_sample_rate);
        self.if_min_hz = min_hz;
        self.if_max_hz = max_hz;
        self.demod_filter.set_fir_bandpass(
            self.if_min_hz / self.coarse_stage_rate,
            self.if_max_hz / self.coarse_stage_rate,
        );
    }

    /// FFT表示窓（開始binと幅）を設定する
    pub fn set_fft_view(&mut self, start_bin: usize, visible_bins: usize) {
        let (start, len) = sanitize_fft_view(self.fft.get_n(), start_bin, visible_bins);
        self.fft_visible_start = start;
        self.fft_visible_len = len;
        self.fft_visible_buffer.resize(len, -120.0);
    }

    /// 複素IQのDCキャンセルを有効/無効にする
    pub fn set_dc_cancel_enabled(&mut self, enabled: bool) {
        self.dc_cancel_enabled = enabled;
    }

    /// FFT入力を処理済みIQに切り替える（falseで生IQ）
    pub fn set_fft_use_processed(&mut self, enabled: bool) {
        self.fft_use_processed = enabled;
    }

    /// 1ブロックのIQデータ(i8型)を受け取り、オーディオ信号とFFT結果を返す。
    pub fn process(&mut self, iq_data: &[i8]) -> js_sys::Array {
        let num_samples = iq_data.len() / 2;
        let fft_n = self.fft.get_n();

        self.baseband_buffer.clear();
        self.baseband_buffer.reserve(num_samples);

        // ベースバンド処理 & NCO
        for (idx, iq) in iq_data.chunks_exact(2).enumerate() {
            let i_val = iq[0] as f32 / 128.0;
            let q_val = iq[1] as f32 / 128.0;
            let raw_sample = Complex::new(i_val, q_val);
            let dc_cancelled = self.dc_canceller.process(raw_sample);
            let sample = if self.dc_cancel_enabled {
                dc_cancelled
            } else {
                raw_sample
            };
            let nco_val = self.nco.step();
            self.baseband_buffer.push(sample * nco_val);

            if self.fft_use_processed && idx < fft_n {
                let n = idx * 2;
                self.fft_input_buffer[n] = float_to_i8(sample.re);
                self.fft_input_buffer[n + 1] = float_to_i8(sample.im);
            }
        }

        // デシメーション (粗段: rx->1Msps, 固定段: 1Msps->demod_rate)
        self.coarse_filter
            .process_into(&self.baseband_buffer, &mut self.coarse_buffer);
        self.demod_filter
            .process_into(&self.coarse_buffer, &mut self.demod_iq_buffer);

        // 復調（モードに応じて分岐）
        self.demod_buffer.resize(self.demod_iq_buffer.len(), 0.0);
        match self.mode {
            DemodMode::Am => self
                .am_demod
                .demodulate(&self.demod_iq_buffer, &mut self.demod_buffer),
            DemodMode::Fm => self
                .fm_demod
                .demodulate(&self.demod_iq_buffer, &mut self.demod_buffer),
        }

        // リサンプリング (demod_rate -> audioCtx.sampleRate)
        self.audio_buffer.clear();
        self.audio_buffer.reserve(
            ((self.demod_buffer.len() as f32 / self.resampler.source_rate as f32)
                * self.resampler.target_rate as f32
                * 1.5) as usize,
        );
        self.resampler
            .process(&self.demod_buffer, &mut self.audio_buffer);

        // デバッグログ（最初の5ブロックのみ）
        {
            static LOG_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let count = LOG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if count < 5 {
                // NCO後のDC成分（信号がDCにあるか検証）
                let bb_dc = self.baseband_buffer.iter().sum::<Complex<f32>>()
                    / self.baseband_buffer.len().max(1) as f32;
                let dec_peak = self
                    .demod_iq_buffer
                    .iter()
                    .map(|s| s.norm())
                    .fold(0.0f32, f32::max);
                let demod_peak = self
                    .demod_buffer
                    .iter()
                    .map(|s| s.abs())
                    .fold(0.0f32, f32::max);
                log(&format!(
                    "[process#{}] bb_dc={:.4}+{:.4}j (mag={:.4}) dec_peak={:.4} demod_peak={:.4} audio_len={}",
                    count, bb_dc.re, bb_dc.im, bb_dc.norm(), dec_peak, demod_peak, self.audio_buffer.len()
                ));
            }
        }

        // FFT (iq_data の先頭 fft_size * 2 要素を使用)
        self.fft_buffer.fill(-120.0);
        if self.fft_use_processed {
            if num_samples >= fft_n {
                self.fft
                    .fft(&self.fft_input_buffer[0..fft_n * 2], &mut self.fft_buffer);
            }
        } else if iq_data.len() >= fft_n * 2 {
            self.fft.fft(&iq_data[0..fft_n * 2], &mut self.fft_buffer);
        }
        let visible_end = self.fft_visible_start + self.fft_visible_len;
        self.fft_visible_buffer
            .copy_from_slice(&self.fft_buffer[self.fft_visible_start..visible_end]);

        let out_array = js_sys::Array::new();
        out_array.push(&js_sys::Float32Array::from(self.audio_buffer.as_slice()));
        out_array.push(&js_sys::Float32Array::from(
            self.fft_visible_buffer.as_slice(),
        ));

        out_array
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32) {
        let diff = (a - b).abs();
        assert!(diff < 1e-3, "lhs={} rhs={} diff={}", a, b, diff);
    }

    #[test]
    fn decimation_plan_fits_candidate_rates_for_am() {
        for (sr, expected_coarse) in [
            (2_000_000.0, 2usize),
            (4_000_000.0, 4usize),
            (8_000_000.0, 8usize),
            (10_000_000.0, 10usize),
            (20_000_000.0, 20usize),
        ] {
            let plan = build_decimation_plan(sr, DemodMode::Am);
            assert_eq!(plan.coarse_factor, expected_coarse);
            assert_eq!(plan.demod_factor, 20);
            approx_eq(plan.coarse_stage_rate, 1_000_000.0);
            approx_eq(plan.demod_sample_rate, AM_DEMOD_RATE);
        }
    }

    #[test]
    fn decimation_plan_fits_candidate_rates_for_fm() {
        for sr in [
            2_000_000.0,
            4_000_000.0,
            8_000_000.0,
            10_000_000.0,
            20_000_000.0,
        ] {
            let plan = build_decimation_plan(sr, DemodMode::Fm);
            assert_eq!(plan.demod_factor, 5);
            approx_eq(plan.coarse_stage_rate, 1_000_000.0);
            approx_eq(plan.demod_sample_rate, FM_DEMOD_RATE);
        }
    }

    #[test]
    fn sanitize_if_band_is_bounded_by_demod_rate() {
        let (_, max) = sanitize_if_band(0.0, 200_000.0, AM_DEMOD_RATE);
        assert!(max <= AM_DEMOD_RATE * 0.49);
    }
}
