#![deny(warnings)]
#![deny(clippy::all)]

pub mod demod;
mod fft;
mod filter;
mod resample;

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
use std::arch::wasm32::{
    f32x4_add, f32x4_convert_i32x4, f32x4_mul, f32x4_splat, f32x4_sub,
    i16x8_extend_high_i8x16, i16x8_extend_low_i8x16, i32x4_extend_high_i16x8,
    i32x4_extend_low_i16x8, i32x4_shuffle, v128, v128_load, v128_store,
};
use num_complex::Complex;
use wasm_bindgen::prelude::*;

use crate::demod::{AMDemodulator, FMDemodulator, FMStereoDecoder, FMStereoStats, Nco};
use crate::fft::FFT;
use crate::filter::DecimationFilter;
use crate::resample::Resampler;

/// FM の最大周波数偏移 [Hz]。WFM（ワイドFM放送）想定。
const FM_MAX_DEVIATION_HZ: f32 = 75_000.0;

/// モード別の復調レート [Hz]
const AM_DEMOD_RATE: f32 = 50_000.0;
const FM_DEMOD_RATE: f32 = 200_000.0;
const AM_AUDIO_CUTOFF_HZ: f32 = 5_000.0;
const FM_AUDIO_CUTOFF_HZ: f32 = 15_000.0;
const FM_DEEMPHASIS_TAU_US: f32 = 50.0;
const COARSE_STAGE_RATE: f32 = 1_000_000.0;
const FFT_DC_INTERP_HALF_WIDTH: usize = 2;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

#[cfg(not(target_arch = "wasm32"))]
fn log(_s: &str) {}

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
    coarse_factor: usize,
    demod_factor: usize,
    coarse_stage_rate: f32,
    demod_sample_rate: f32,
    if_min_hz: f32,
    if_max_hz: f32,
    dc_cancel_enabled: bool,
    fm_stereo_enabled: bool,
    adc_peak: f32,
    fft_visible_start: usize,
    fft_visible_len: usize,
    mode: DemodMode,
    nco: Nco,
    coarse_filter: DecimationFilter,
    demod_filter: DecimationFilter,
    am_demod: AMDemodulator,
    fm_demod: FMDemodulator,
    fm_stereo: FMStereoDecoder,
    resampler: Resampler,
    resampler_right: Resampler,
    fft: FFT,

    // 中間バッファ（アロケーションを避けるため保持）
    baseband_buffer: Vec<Complex<f32>>,
    coarse_buffer: Vec<Complex<f32>>,
    demod_iq_buffer: Vec<Complex<f32>>,
    demod_buffer: Vec<f32>, // AM: baseband audio / FM: MPX
    stereo_left_buffer: Vec<f32>,
    stereo_right_buffer: Vec<f32>,
    audio_left_resampled: Vec<f32>,
    audio_right_resampled: Vec<f32>,
    audio_buffer: Vec<f32>,
    fft_buffer: Vec<f32>,
    fft_visible_buffer: Vec<f32>,
    io_iq_buffer: Vec<i8>,
    io_audio_buffer: Vec<f32>,
    io_fft_buffer: Vec<f32>,
}

#[wasm_bindgen]
pub struct ReceiverStats {
    fm_stereo_pilot_level: f32,
    fm_stereo_blend: f32,
    fm_stereo_locked: bool,
    fm_stereo_mono_fallback_count: u32,
    fm_stereo_pll_phase_err_rad: f32,
    fm_stereo_pll_freq_corr_hz: f32,
    fm_stereo_pll_q_over_i: f32,
    fm_stereo_pll_locked: bool,
    adc_peak: f32,
}

#[wasm_bindgen]
impl ReceiverStats {
    #[wasm_bindgen(getter)]
    pub fn fm_stereo_pilot_level(&self) -> f32 {
        self.fm_stereo_pilot_level
    }

    #[wasm_bindgen(getter)]
    pub fn fm_stereo_blend(&self) -> f32 {
        self.fm_stereo_blend
    }

    #[wasm_bindgen(getter)]
    pub fn fm_stereo_locked(&self) -> bool {
        self.fm_stereo_locked
    }

    #[wasm_bindgen(getter)]
    pub fn fm_stereo_mono_fallback_count(&self) -> u32 {
        self.fm_stereo_mono_fallback_count
    }

    #[wasm_bindgen(getter)]
    pub fn fm_stereo_pll_phase_err_rad(&self) -> f32 {
        self.fm_stereo_pll_phase_err_rad
    }

    #[wasm_bindgen(getter)]
    pub fn fm_stereo_pll_freq_corr_hz(&self) -> f32 {
        self.fm_stereo_pll_freq_corr_hz
    }

    #[wasm_bindgen(getter)]
    pub fn fm_stereo_pll_q_over_i(&self) -> f32 {
        self.fm_stereo_pll_q_over_i
    }

    #[wasm_bindgen(getter)]
    pub fn fm_stereo_pll_locked(&self) -> bool {
        self.fm_stereo_pll_locked
    }

    #[wasm_bindgen(getter)]
    pub fn adc_peak(&self) -> f32 {
        self.adc_peak
    }
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

fn interpolate_fft_dc_bins(fft_db: &mut [f32], half_width: usize) {
    let n = fft_db.len();
    if n < 2 * half_width + 3 {
        return;
    }
    let dc = n / 2;
    if dc <= half_width || dc + half_width + 1 >= n {
        return;
    }

    let left_idx = dc - half_width - 1;
    let right_idx = dc + half_width + 1;
    let left = fft_db[left_idx];
    let right = fft_db[right_idx];
    let slope = (right - left) / (right_idx - left_idx) as f32;

    for idx in (dc - half_width)..=(dc + half_width) {
        let t = (idx - left_idx) as f32;
        fft_db[idx] = left + slope * t;
    }
}

#[inline]
fn block_adc_peak(iq_data: &[i8]) -> f32 {
    iq_data
        .iter()
        .fold(0.0f32, |peak, &v| peak.max((v as f32).abs()))
}

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline]
unsafe fn unpack_iq16_to_f32x4x4_simd(src_iq: *const i8) -> (v128, v128, v128, v128) {
    let scale = f32x4_splat(1.0 / 128.0);
    let packed = v128_load(src_iq as *const v128);
    let lo_i16 = i16x8_extend_low_i8x16(packed);
    let hi_i16 = i16x8_extend_high_i8x16(packed);

    let f0 = f32x4_mul(f32x4_convert_i32x4(i32x4_extend_low_i16x8(lo_i16)), scale);
    let f1 = f32x4_mul(f32x4_convert_i32x4(i32x4_extend_high_i16x8(lo_i16)), scale);
    let f2 = f32x4_mul(f32x4_convert_i32x4(i32x4_extend_low_i16x8(hi_i16)), scale);
    let f3 = f32x4_mul(f32x4_convert_i32x4(i32x4_extend_high_i16x8(hi_i16)), scale);
    (f0, f1, f2, f3)
}

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline]
unsafe fn complex_mul_interleaved2_f32_simd(input: v128, osc: v128) -> v128 {
    let osc_swapped = i32x4_shuffle::<1, 0, 3, 2>(osc, osc);

    let prod_re = f32x4_mul(input, osc);
    let prod_im = f32x4_mul(input, osc_swapped);

    let prod_re_swapped = i32x4_shuffle::<1, 0, 3, 2>(prod_re, prod_re);
    let prod_im_swapped = i32x4_shuffle::<1, 0, 3, 2>(prod_im, prod_im);

    let re = f32x4_sub(prod_re, prod_re_swapped);
    let im = f32x4_add(prod_im, prod_im_swapped);

    i32x4_shuffle::<0, 4, 2, 6>(re, im)
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
        let mut fm_demod = FMDemodulator::new(FM_MAX_DEVIATION_HZ, plan.demod_sample_rate);
        fm_demod.set_deemphasis_tau_us(plan.demod_sample_rate, Some(FM_DEEMPHASIS_TAU_US));
        fm_demod.set_deemphasis_enabled(false);

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

        let audio_cutoff_hz = match mode {
            DemodMode::Am => AM_AUDIO_CUTOFF_HZ,
            DemodMode::Fm => FM_AUDIO_CUTOFF_HZ,
        };

        let resampler = Resampler::new_with_cutoff(
            plan.demod_sample_rate.round() as u32,
            output_sample_rate.round() as u32,
            Some(audio_cutoff_hz),
        );
        let resampler_right = Resampler::new_with_cutoff(
            plan.demod_sample_rate.round() as u32,
            output_sample_rate.round() as u32,
            Some(audio_cutoff_hz),
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
            coarse_factor: plan.coarse_factor,
            demod_factor: plan.demod_factor,
            coarse_stage_rate: plan.coarse_stage_rate,
            demod_sample_rate: plan.demod_sample_rate,
            if_min_hz,
            if_max_hz,
            dc_cancel_enabled,
            fm_stereo_enabled: true,
            adc_peak: 0.0,
            fft_visible_start,
            fft_visible_len,
            mode,
            nco: Nco::new(-offset_hz, sample_rate),
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
            fm_demod,
            fm_stereo: FMStereoDecoder::new(plan.demod_sample_rate, Some(FM_DEEMPHASIS_TAU_US)),
            resampler,
            resampler_right,
            fft: FFT::new(fft_size, &window),
            baseband_buffer: Vec::with_capacity(131_072),
            coarse_buffer: Vec::with_capacity(131_072),
            demod_iq_buffer: Vec::with_capacity(131_072),
            demod_buffer: Vec::with_capacity(8_192),
            stereo_left_buffer: Vec::with_capacity(8_192),
            stereo_right_buffer: Vec::with_capacity(8_192),
            audio_left_resampled: Vec::with_capacity(8_192),
            audio_right_resampled: Vec::with_capacity(8_192),
            audio_buffer: Vec::with_capacity(8_192),
            fft_buffer: vec![0.0; fft_size],
            fft_visible_buffer: vec![-120.0; fft_visible_len],
            io_iq_buffer: Vec::new(),
            io_audio_buffer: Vec::new(),
            io_fft_buffer: Vec::new(),
        }
    }

    /// 受信対象の周波数（あるいはオフセット）を変更する
    pub fn set_target_freq(&mut self, center_freq: f32, target_freq: f32) {
        let offset_hz = target_freq - center_freq;
        self.nco = Nco::new(-offset_hz, self.sample_rate);
        if self.mode == DemodMode::Fm {
            // リチューン時はFM復調・ステレオ判定の状態を引きずらない。
            self.fm_demod = FMDemodulator::new(FM_MAX_DEVIATION_HZ, self.demod_sample_rate);
            self.fm_demod
                .set_deemphasis_tau_us(self.demod_sample_rate, Some(FM_DEEMPHASIS_TAU_US));
            self.fm_demod
                .set_deemphasis_enabled(!self.fm_stereo_enabled);
            self.fm_stereo.reset();
        }
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
        if self.mode == DemodMode::Fm {
            // IF帯域変更時も過去状態が混ざらないようにリセットする。
            self.fm_demod = FMDemodulator::new(FM_MAX_DEVIATION_HZ, self.demod_sample_rate);
            self.fm_demod
                .set_deemphasis_tau_us(self.demod_sample_rate, Some(FM_DEEMPHASIS_TAU_US));
            self.fm_demod
                .set_deemphasis_enabled(!self.fm_stereo_enabled);
            self.fm_stereo.reset();
        }
    }

    /// FFT表示窓（開始binと幅）を設定する
    pub fn set_fft_view(&mut self, start_bin: usize, visible_bins: usize) {
        let (start, len) = sanitize_fft_view(self.fft.get_n(), start_bin, visible_bins);
        self.fft_visible_start = start;
        self.fft_visible_len = len;
        self.fft_visible_buffer.resize(len, -120.0);
    }

    /// FFT表示の DC 近傍補間を有効/無効にする
    pub fn set_dc_cancel_enabled(&mut self, enabled: bool) {
        self.dc_cancel_enabled = enabled;
    }

    /// FMステレオ復調の有効/無効を設定する（無効時はFMを強制MONOで出力）
    pub fn set_fm_stereo_enabled(&mut self, enabled: bool) {
        if self.fm_stereo_enabled == enabled {
            return;
        }
        self.fm_stereo_enabled = enabled;
        if self.mode == DemodMode::Fm {
            self.fm_stereo.reset();
            self.fm_demod.set_deemphasis_enabled(!enabled);
            self.fm_demod.reset_audio_state();
        }
    }

    /// JS から直接読み書きするI/Oバッファを初期化する。
    pub fn alloc_io_buffers(
        &mut self,
        max_iq_bytes: usize,
        max_audio_samples: usize,
        max_fft_bins: usize,
    ) -> Result<(), JsValue> {
        if max_iq_bytes == 0 || (max_iq_bytes & 1) != 0 {
            return Err(JsValue::from_str("max_iq_bytes must be even and > 0"));
        }
        if max_audio_samples == 0 {
            return Err(JsValue::from_str("max_audio_samples must be > 0"));
        }
        if max_fft_bins < self.fft_visible_len {
            return Err(JsValue::from_str("max_fft_bins is smaller than visible FFT bins"));
        }

        self.io_iq_buffer = vec![0; max_iq_bytes];
        self.io_audio_buffer = vec![0.0; max_audio_samples];
        self.io_fft_buffer = vec![0.0; max_fft_bins];

        let max_iq_samples = max_iq_bytes / 2;
        let max_coarse = max_iq_samples / self.coarse_factor + 2;
        let max_demod = max_coarse / self.demod_factor + 2;

        self.baseband_buffer.reserve(max_iq_samples);
        self.coarse_buffer.reserve(max_coarse);
        self.demod_iq_buffer.reserve(max_demod);
        self.demod_buffer.reserve(max_demod);
        self.stereo_left_buffer.reserve(max_demod);
        self.stereo_right_buffer.reserve(max_demod);
        self.audio_left_resampled.reserve(max_audio_samples / 2 + 2);
        self.audio_right_resampled.reserve(max_audio_samples / 2 + 2);
        self.audio_buffer.reserve(max_audio_samples);
        Ok(())
    }

    pub fn free_io_buffers(&mut self) {
        self.io_iq_buffer.clear();
        self.io_iq_buffer.shrink_to_fit();
        self.io_audio_buffer.clear();
        self.io_audio_buffer.shrink_to_fit();
        self.io_fft_buffer.clear();
        self.io_fft_buffer.shrink_to_fit();
    }

    pub fn iq_input_ptr(&self) -> u32 {
        self.io_iq_buffer.as_ptr() as usize as u32
    }

    pub fn audio_output_ptr(&self) -> u32 {
        self.io_audio_buffer.as_ptr() as usize as u32
    }

    pub fn fft_output_ptr(&self) -> u32 {
        self.io_fft_buffer.as_ptr() as usize as u32
    }

    pub fn iq_input_capacity(&self) -> usize {
        self.io_iq_buffer.len()
    }

    pub fn audio_output_capacity(&self) -> usize {
        self.io_audio_buffer.len()
    }

    pub fn fft_output_capacity(&self) -> usize {
        self.io_fft_buffer.len()
    }

    pub fn audio_output_channels(&self) -> usize {
        match self.mode {
            DemodMode::Am => 1,
            DemodMode::Fm => 2,
        }
    }

    pub fn get_stats(&self) -> ReceiverStats {
        let stereo = if self.mode == DemodMode::Fm && self.fm_stereo_enabled {
            self.fm_stereo.stats()
        } else {
            FMStereoStats::default()
        };
        ReceiverStats {
            fm_stereo_pilot_level: stereo.pilot_level,
            fm_stereo_blend: stereo.stereo_blend,
            fm_stereo_locked: stereo.stereo_locked,
            fm_stereo_mono_fallback_count: stereo.mono_fallback_count,
            fm_stereo_pll_phase_err_rad: stereo.pll_phase_err_rad,
            fm_stereo_pll_freq_corr_hz: stereo.pll_freq_corr_hz,
            fm_stereo_pll_q_over_i: stereo.pll_q_over_i,
            fm_stereo_pll_locked: stereo.pll_locked,
            adc_peak: self.adc_peak,
        }
    }

    /// `alloc_io_buffers` で確保したI/Qバッファ先頭 `iq_len` バイトを入力として処理する。
    /// `want_fft` が false の場合は FFT 更新をスキップする。
    pub fn process_iq_len(&mut self, iq_len: usize, want_fft: bool) -> Result<usize, JsValue> {
        if self.io_iq_buffer.is_empty() || self.io_audio_buffer.is_empty() || self.io_fft_buffer.is_empty() {
            return Err(JsValue::from_str("io buffers are not allocated"));
        }
        if iq_len == 0 || (iq_len & 1) != 0 {
            return Err(JsValue::from_str("iq_len must be even and > 0"));
        }
        if iq_len > self.io_iq_buffer.len() {
            return Err(JsValue::from_str("iq_len exceeds io_iq_buffer capacity"));
        }

        // io_iq_buffer の先頭領域を入力として読むだけで、process_internal では
        // io_iq_buffer を変更しないため、raw pointer からの一時スライス化で確保を避ける。
        let iq_ptr = self.io_iq_buffer.as_ptr();
        let iq_slice = unsafe { std::slice::from_raw_parts(iq_ptr, iq_len) };
        self.process_internal(iq_slice, want_fft);

        if self.audio_buffer.len() > self.io_audio_buffer.len() {
            return Err(JsValue::from_str("audio output exceeds io_audio_buffer capacity"));
        }

        let audio_len = self.audio_buffer.len();
        if audio_len > 0 {
            self.io_audio_buffer[..audio_len].copy_from_slice(&self.audio_buffer[..audio_len]);
        }
        if want_fft {
            if self.fft_visible_buffer.len() > self.io_fft_buffer.len() {
                return Err(JsValue::from_str("fft output exceeds io_fft_buffer capacity"));
            }
            let fft_len = self.fft_visible_buffer.len();
            if fft_len > 0 {
                self.io_fft_buffer[..fft_len].copy_from_slice(&self.fft_visible_buffer[..fft_len]);
            }
        }
        Ok(audio_len)
    }

    /// 1ブロックのIQデータ(i8型)を受け取り、指定バッファへ結果を書き込む。
    /// 返り値は `audio_out` に有効に書き込まれたサンプル数。
    pub fn process_into(&mut self, iq_data: &[i8], audio_out: &mut [f32], fft_out: &mut [f32]) -> usize {
        self.process_internal(iq_data, true);

        let audio_len = self.audio_buffer.len().min(audio_out.len());
        if audio_len > 0 {
            audio_out[..audio_len].copy_from_slice(&self.audio_buffer[..audio_len]);
        }

        let fft_len = self.fft_visible_buffer.len().min(fft_out.len());
        if fft_len > 0 {
            fft_out[..fft_len].copy_from_slice(&self.fft_visible_buffer[..fft_len]);
        }

        audio_len
    }

}

impl Receiver {
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    fn mix_iq_to_baseband(&mut self, iq_data: &[i8]) {
        self.adc_peak = block_adc_peak(iq_data);
        let num_samples = iq_data.len() / 2;
        self.baseband_buffer
            .resize(num_samples, Complex::new(0.0, 0.0));

        let mut idx = 0usize;
        while idx + 8 <= num_samples {
            let (x0, x1, x2, x3) =
                unsafe { unpack_iq16_to_f32x4x4_simd(iq_data.as_ptr().add(idx * 2)) };
            let (n0, n1, n2, n3) = self.nco.step8_interleaved();

            let y0 = unsafe { complex_mul_interleaved2_f32_simd(x0, n0) };
            let y1 = unsafe { complex_mul_interleaved2_f32_simd(x1, n1) };
            let y2 = unsafe { complex_mul_interleaved2_f32_simd(x2, n2) };
            let y3 = unsafe { complex_mul_interleaved2_f32_simd(x3, n3) };

            unsafe {
                let out_f32 = self.baseband_buffer.as_mut_ptr().add(idx) as *mut f32;
                v128_store(out_f32 as *mut v128, y0);
                v128_store(out_f32.add(4) as *mut v128, y1);
                v128_store(out_f32.add(8) as *mut v128, y2);
                v128_store(out_f32.add(12) as *mut v128, y3);
            }
            idx += 8;
        }

        for out_idx in idx..num_samples {
            let base = out_idx * 2;
            let sample = Complex::new(iq_data[base] as f32 / 128.0, iq_data[base + 1] as f32 / 128.0);
            let nco_val = self.nco.step();
            self.baseband_buffer[out_idx] = sample * nco_val;
        }
    }

    #[cfg(not(all(target_arch = "wasm32", target_feature = "simd128")))]
    fn mix_iq_to_baseband(&mut self, iq_data: &[i8]) {
        self.adc_peak = block_adc_peak(iq_data);
        self.baseband_buffer.clear();
        self.baseband_buffer.reserve(iq_data.len() / 2);
        for iq in iq_data.chunks_exact(2) {
            let sample = Complex::new(iq[0] as f32 / 128.0, iq[1] as f32 / 128.0);
            let nco_val = self.nco.step();
            self.baseband_buffer.push(sample * nco_val);
        }
    }

    fn process_internal(&mut self, iq_data: &[i8], want_fft: bool) {
        let fft_n = self.fft.get_n();

        // ベースバンド処理 & NCO
        self.mix_iq_to_baseband(iq_data);

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

        match self.mode {
            DemodMode::Am => {
                // リサンプリング (demod_rate -> audioCtx.sampleRate)
                self.audio_buffer.clear();
                self.audio_buffer.reserve(
                    ((self.demod_buffer.len() as f32 / self.resampler.source_rate as f32)
                        * self.resampler.target_rate as f32
                        * 1.5) as usize,
                );
                self.resampler
                    .process(&self.demod_buffer, &mut self.audio_buffer);
            }
            DemodMode::Fm => {
                if self.fm_stereo_enabled {
                    // FMは MPX -> Stereo Decode -> resample(L/R) -> interleave
                    self.fm_stereo.process(
                        &self.demod_buffer,
                        &mut self.stereo_left_buffer,
                        &mut self.stereo_right_buffer,
                    );

                    self.audio_left_resampled.clear();
                    self.audio_right_resampled.clear();
                    self.resampler
                        .process(&self.stereo_left_buffer, &mut self.audio_left_resampled);
                    self.resampler_right
                        .process(&self.stereo_right_buffer, &mut self.audio_right_resampled);

                    let frames = self
                        .audio_left_resampled
                        .len()
                        .min(self.audio_right_resampled.len());
                    self.audio_buffer.clear();
                    self.audio_buffer.reserve(frames * 2);
                    for i in 0..frames {
                        self.audio_buffer.push(self.audio_left_resampled[i]);
                        self.audio_buffer.push(self.audio_right_resampled[i]);
                    }
                } else {
                    // FMステレオ無効時はFM復調器側のdeemphasis済みmonoを左右同一出力。
                    self.audio_left_resampled.clear();
                    self.resampler
                        .process(&self.demod_buffer, &mut self.audio_left_resampled);
                    self.audio_buffer.clear();
                    self.audio_buffer.reserve(self.audio_left_resampled.len() * 2);
                    for &sample in &self.audio_left_resampled {
                        self.audio_buffer.push(sample);
                        self.audio_buffer.push(sample);
                    }
                }
            }
        }

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

        if want_fft {
            // FFT (iq_data の先頭 fft_size * 2 要素を使用)
            self.fft_buffer.fill(-120.0);
            if iq_data.len() >= fft_n * 2 {
                self.fft.fft(&iq_data[0..fft_n * 2], &mut self.fft_buffer);
                if self.dc_cancel_enabled {
                    interpolate_fft_dc_bins(&mut self.fft_buffer, FFT_DC_INTERP_HALF_WIDTH);
                }
            }
            let visible_end = self.fft_visible_start + self.fft_visible_len;
            self.fft_visible_buffer
                .copy_from_slice(&self.fft_buffer[self.fft_visible_start..visible_end]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32) {
        let diff = (a - b).abs();
        assert!(diff < 1e-3, "lhs={} rhs={} diff={}", a, b, diff);
    }

    fn build_stereo_mpx(fs_hz: f32, len: usize, left_hz: f32, right_hz: f32) -> Vec<f32> {
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            let t = i as f32 / fs_hz;
            let l = 0.5 * (2.0 * std::f32::consts::PI * left_hz * t).sin();
            let r = 0.5 * (2.0 * std::f32::consts::PI * right_hz * t).sin();
            let lp = l + r;
            let lr = l - r;
            let pilot = 0.10 * (2.0 * std::f32::consts::PI * 19_000.0 * t).cos();
            let dsb = lr * (2.0 * std::f32::consts::PI * 38_000.0 * t).cos();
            out.push(0.45 * lp + pilot + 0.45 * dsb);
        }
        out
    }

    fn fm_modulate_to_i8_iq(mpx: &[f32], fs_hz: f32, max_dev_hz: f32) -> Vec<i8> {
        let mut out = Vec::with_capacity(mpx.len() * 2);
        let mut phase = 0.0f32;
        let scale = 0.8 * 127.0;
        for &x in mpx {
            let inst_freq = max_dev_hz * x;
            phase += 2.0 * std::f32::consts::PI * inst_freq / fs_hz;
            if phase > std::f32::consts::PI {
                phase -= 2.0 * std::f32::consts::TAU;
            } else if phase < -std::f32::consts::PI {
                phase += std::f32::consts::TAU;
            }
            let i = (phase.cos() * scale).round().clamp(-127.0, 127.0) as i8;
            let q = (phase.sin() * scale).round().clamp(-127.0, 127.0) as i8;
            out.push(i);
            out.push(q);
        }
        out
    }

    fn run_fm_receiver_audio(iq: &[i8], if_max_hz: f32) -> (Vec<f32>, ReceiverStats) {
        let mut rx = Receiver::new(
            2_000_000.0,
            100_000_000.0,
            100_000_000.0,
            "FM",
            48_000.0,
            1024,
            0,
            1024,
            0.0,
            if_max_hz,
            true,
        );
        rx.set_fm_stereo_enabled(true);

        let mut audio_all = Vec::<f32>::new();
        let mut audio_out = vec![0.0f32; 8192];
        let mut fft_out = vec![0.0f32; 1024];
        let block_bytes = 16_384usize;
        for chunk in iq.chunks(block_bytes) {
            let n = rx.process_into(chunk, &mut audio_out, &mut fft_out);
            audio_all.extend_from_slice(&audio_out[..n]);
        }
        (audio_all, rx.get_stats())
    }

    fn split_stereo_interleaved(samples: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let frames = samples.len() / 2;
        let mut left = Vec::with_capacity(frames);
        let mut right = Vec::with_capacity(frames);
        for frame in samples.chunks_exact(2) {
            left.push(frame[0]);
            right.push(frame[1]);
        }
        (left, right)
    }

    fn tone_amp(signal: &[f32], fs_hz: f32, freq_hz: f32) -> f32 {
        if signal.is_empty() {
            return 0.0;
        }
        let w = 2.0 * std::f32::consts::PI * freq_hz / fs_hz;
        let mut re = 0.0f32;
        let mut im = 0.0f32;
        for (n, &x) in signal.iter().enumerate() {
            let phi = w * n as f32;
            re += x * phi.cos();
            im -= x * phi.sin();
        }
        2.0 * (re.hypot(im)) / signal.len() as f32
    }

    fn separation_db(main: f32, leak: f32) -> f32 {
        let num = main.abs().max(1e-9);
        let den = leak.abs().max(1e-9);
        20.0 * (num / den).log10()
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

    #[test]
    fn sanitize_if_band_repairs_invalid_input() {
        let demod_rate = 50_000.0;
        let (min_hz, max_hz) = sanitize_if_band(-10_000.0, -1.0, demod_rate);
        assert!(min_hz >= 0.0);
        assert!(max_hz > min_hz);
        assert!(max_hz <= demod_rate * 0.49);

        let (min_hz2, max_hz2) = sanitize_if_band(1_000_000.0, 1_000_100.0, demod_rate);
        assert_eq!(min_hz2, 0.0);
        assert!(max_hz2 <= demod_rate * 0.49);
        assert!(max_hz2 > 0.0);
    }

    #[test]
    fn sanitize_fft_view_clamps_start_and_len() {
        let (start, len) = sanitize_fft_view(1024, 999_999, 0);
        assert_eq!(start, 1023);
        assert_eq!(len, 1);

        let (start2, len2) = sanitize_fft_view(1024, 1000, 4096);
        assert_eq!(start2, 1000);
        assert_eq!(len2, 24);
    }

    #[test]
    fn interpolate_fft_dc_bins_is_noop_for_short_input() {
        let mut bins = vec![-120.0f32; 6];
        let before = bins.clone();
        interpolate_fft_dc_bins(&mut bins, FFT_DC_INTERP_HALF_WIDTH);
        assert_eq!(bins, before);
    }

    #[test]
    fn interpolate_fft_dc_bins_fills_linear_bridge() {
        let mut bins = vec![
            -90.0, -80.0, -70.0, -60.0, -50.0, // left side
            10.0, 20.0, 30.0, 40.0, 50.0, // dc neighborhood to overwrite
            -40.0, -30.0, -20.0, -10.0, 0.0, // right side
        ];
        interpolate_fft_dc_bins(&mut bins, 2);
        // n=15 -> dc=7, overwritten range 5..=9, endpoints idx4=-50 idx10=-40
        let expected = [-48.333332, -46.666668, -45.0, -43.333332, -41.666668];
        for (idx, exp) in (5usize..=9).zip(expected.iter()) {
            assert!((bins[idx] - exp).abs() < 1e-4, "idx={} got={} expected={}", idx, bins[idx], exp);
        }
    }

    #[test]
    fn block_adc_peak_returns_max_abs_value() {
        let iq = [0i8, -3, 12, -45, 44, 9, -1, 1];
        approx_eq(block_adc_peak(&iq), 45.0);
    }

    #[test]
    fn receiver_stats_adc_peak_tracks_latest_block() {
        let mut rx = Receiver::new(
            2_000_000.0,
            100_000_000.0,
            100_000_000.0,
            "AM",
            48_000.0,
            1024,
            0,
            1024,
            0.0,
            4_500.0,
            true,
        );
        let mut audio_out = vec![0.0f32; 8192];
        let mut fft_out = vec![0.0f32; 1024];

        let mut block1 = vec![0i8; 4096];
        block1[17] = -45;
        rx.process_into(&block1, &mut audio_out, &mut fft_out);
        approx_eq(rx.get_stats().adc_peak(), 45.0);

        let mut block2 = vec![0i8; 4096];
        block2[29] = -128;
        rx.process_into(&block2, &mut audio_out, &mut fft_out);
        approx_eq(rx.get_stats().adc_peak(), 128.0);
    }

    #[test]
    fn fm_pipeline_end_to_end_stereo_separation_is_observable() {
        let fs = 2_000_000.0f32;
        let n = 1_000_000usize;
        let mpx = build_stereo_mpx(fs, n, 1_000.0, 2_000.0);
        let iq = fm_modulate_to_i8_iq(&mpx, fs, FM_MAX_DEVIATION_HZ);

        let (audio, stats) = run_fm_receiver_audio(&iq, 98_000.0);
        assert!(
            stats.fm_stereo_blend() > 0.50,
            "stereo blend too low: {}",
            stats.fm_stereo_blend()
        );
        assert!(stats.fm_stereo_locked(), "stereo did not lock in e2e pipeline");

        let (left, right) = split_stereo_interleaved(&audio);
        assert!(left.len() > 8_000, "too few audio samples: {}", left.len());

        // 立ち上がり過渡を捨てる
        let skip = left.len() / 4;
        let l = &left[skip..];
        let r = &right[skip..];

        let l_main = tone_amp(l, 48_000.0, 1_000.0);
        let l_leak = tone_amp(l, 48_000.0, 2_000.0);
        let r_main = tone_amp(r, 48_000.0, 2_000.0);
        let r_leak = tone_amp(r, 48_000.0, 1_000.0);
        let sep_l = separation_db(l_main, l_leak);
        let sep_r = separation_db(r_main, r_leak);

        assert!(
            sep_l > 3.0,
            "left separation too low in e2e pipeline: sep_l={}dB (main={} leak={})",
            sep_l,
            l_main,
            l_leak
        );
        assert!(
            sep_r > 3.0,
            "right separation too low in e2e pipeline: sep_r={}dB (main={} leak={})",
            sep_r,
            r_main,
            r_leak
        );
    }

    #[test]
    fn fm_pipeline_end_to_end_too_narrow_if_degrades_separation() {
        let fs = 2_000_000.0f32;
        let n = 1_000_000usize;
        // 高めのオーディオ帯域で差を見やすくする。
        let mpx = build_stereo_mpx(fs, n, 9_000.0, 11_000.0);
        let iq = fm_modulate_to_i8_iq(&mpx, fs, FM_MAX_DEVIATION_HZ);

        // 45kHz は FMステレオ用としては明確に狭すぎる設定。
        let (audio_narrow, _) = run_fm_receiver_audio(&iq, 45_000.0);
        let (audio_wide, _) = run_fm_receiver_audio(&iq, 98_000.0);
        let (left_n, right_n) = split_stereo_interleaved(&audio_narrow);
        let (left_w, right_w) = split_stereo_interleaved(&audio_wide);
        let skip_n = left_n.len() / 4;
        let skip_w = left_w.len() / 4;
        let ln = &left_n[skip_n..];
        let rn = &right_n[skip_n..];
        let lw = &left_w[skip_w..];
        let rw = &right_w[skip_w..];

        let sep_n_l = separation_db(tone_amp(ln, 48_000.0, 9_000.0), tone_amp(ln, 48_000.0, 11_000.0));
        let sep_n_r = separation_db(tone_amp(rn, 48_000.0, 11_000.0), tone_amp(rn, 48_000.0, 9_000.0));
        let sep_w_l = separation_db(tone_amp(lw, 48_000.0, 9_000.0), tone_amp(lw, 48_000.0, 11_000.0));
        let sep_w_r = separation_db(tone_amp(rw, 48_000.0, 11_000.0), tone_amp(rw, 48_000.0, 9_000.0));
        let sep_n = 0.5 * (sep_n_l + sep_n_r);
        let sep_w = 0.5 * (sep_w_l + sep_w_r);

        assert!(
            sep_w > sep_n + 1.5,
            "wider IF did not improve e2e separation enough: narrow={}dB wide={}dB",
            sep_n,
            sep_w
        );
    }

    #[cfg(target_arch = "wasm32")]
    fn make_receiver() -> Receiver {
        Receiver::new(
            2_000_000.0,
            100_000_000.0,
            100_000_000.0,
            "AM",
            48_000.0,
            1024,
            0,
            1024,
            0.0,
            4_500.0,
            true,
        )
    }

    #[cfg(target_arch = "wasm32")]
    #[test]
    fn alloc_io_buffers_validates_arguments() {
        let mut receiver = make_receiver();
        assert!(receiver.alloc_io_buffers(0, 1024, 1024).is_err());
        assert!(receiver.alloc_io_buffers(3, 1024, 1024).is_err());
        assert!(receiver.alloc_io_buffers(1024, 0, 1024).is_err());
        assert!(receiver.alloc_io_buffers(1024, 1024, 100).is_err());
    }

    #[cfg(target_arch = "wasm32")]
    #[test]
    fn process_iq_len_requires_alloc_and_valid_len() {
        let mut receiver = make_receiver();
        assert!(receiver.process_iq_len(1024, true).is_err());

        receiver
            .alloc_io_buffers(4096, 4096, 1024)
            .expect("alloc_io_buffers should succeed");
        assert!(receiver.process_iq_len(0, true).is_err());
        assert!(receiver.process_iq_len(3, true).is_err());
        assert!(receiver.process_iq_len(4098, true).is_err());
    }

    #[cfg(target_arch = "wasm32")]
    #[test]
    fn process_iq_len_smoke() {
        let mut receiver = make_receiver();
        receiver
            .alloc_io_buffers(4096, 4096, 1024)
            .expect("alloc_io_buffers should succeed");

        let iq_ptr = receiver.iq_input_ptr() as usize as *mut i8;
        let iq_slice = unsafe { std::slice::from_raw_parts_mut(iq_ptr, 4096) };
        for (i, v) in iq_slice.iter_mut().enumerate() {
            *v = if (i & 1) == 0 { 64 } else { -64 };
        }
        let out = receiver.process_iq_len(4096, true);
        assert!(out.is_ok());
        assert!(out.expect("audio length should be returned") <= receiver.audio_output_capacity());
    }
}
