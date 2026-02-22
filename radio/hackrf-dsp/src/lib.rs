#![deny(warnings)]
#![deny(clippy::all)]

mod demod;
mod filter;
mod fft;
mod resample;

use num_complex::Complex;
use wasm_bindgen::prelude::*;

use crate::demod::{AMDemodulator, Nco};
use crate::fft::FFT;
use crate::filter::DecimationFilter;
use crate::resample::Resampler;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

/// JS側から呼び出されるラジオのメインDSPレシーバ
#[wasm_bindgen]
pub struct Receiver {
    sample_rate: f32,
    decimated_sample_rate: f32,
    if_min_hz: f32,
    if_max_hz: f32,
    nco: Nco,
    filter: DecimationFilter,
    am_demod: AMDemodulator,
    resampler: Resampler,
    fft: FFT,

    // 中間バッファ（アロケーションを避けるため保持）
    baseband_buffer: Vec<Complex<f32>>,
    am_buffer: Vec<f32>,
    audio_buffer: Vec<f32>,
    fft_buffer: Vec<f32>,
}

fn sanitize_if_band(min_hz: f32, max_hz: f32, decimated_sample_rate: f32) -> (f32, f32) {
    let max_allowed = (decimated_sample_rate * 0.49).max(200.0);
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

#[wasm_bindgen]
impl Receiver {
    #[wasm_bindgen(constructor)]
    pub fn new(
        sample_rate: f32,
        center_freq: f32,
        target_freq: f32,
        decimation_factor: usize,
        output_sample_rate: f32,
        fft_size: usize,
        if_min_hz: f32,
        if_max_hz: f32,
    ) -> Self {
        console_error_panic_hook::set_once();

        assert!(decimation_factor > 0, "decimation_factor must be > 0");

        let offset_hz = target_freq - center_freq;
        let decimated_sample_rate = sample_rate / decimation_factor as f32;
        let (if_min_hz, if_max_hz) = sanitize_if_band(if_min_hz, if_max_hz, decimated_sample_rate);

        // 複素周波数変換後の IF チャンネルフィルタ（BPF = LPF(max) - LPF(min)）
        let min_cutoff_norm = if_min_hz / sample_rate;
        let max_cutoff_norm = if_max_hz / sample_rate;

        let resampler = Resampler::new(
            decimated_sample_rate.round() as u32,
            output_sample_rate.round() as u32,
        );

        // FFT窓関数 (Hann窓)
        let mut window = vec![0.0f32; fft_size];
        if fft_size == 1 {
            window[0] = 1.0;
        } else {
            for (i, w) in window.iter_mut().enumerate() {
                *w = 0.5
                    * (1.0
                        - (2.0 * std::f32::consts::PI * i as f32 / (fft_size - 1) as f32).cos());
            }
        }

        Self {
            sample_rate,
            decimated_sample_rate,
            if_min_hz,
            if_max_hz,
            nco: Nco::new(-offset_hz, sample_rate),
            filter: DecimationFilter::new_fir_band(decimation_factor, 601, min_cutoff_norm, max_cutoff_norm),
            am_demod: AMDemodulator::new(),
            resampler,
            fft: FFT::new(fft_size, &window),
            baseband_buffer: Vec::with_capacity(131_072),
            am_buffer: Vec::with_capacity(8_192),
            audio_buffer: Vec::with_capacity(8_192),
            fft_buffer: vec![0.0; fft_size],
        }
    }

    /// 受信対象の周波数（あるいはオフセット）を変更する
    pub fn set_target_freq(&mut self, center_freq: f32, target_freq: f32) {
        let offset_hz = target_freq - center_freq;
        self.nco = Nco::new(-offset_hz, self.sample_rate);
    }

    /// IFチャンネルフィルタの通過帯域を変更する（Hz）
    pub fn set_if_band(&mut self, min_hz: f32, max_hz: f32) {
        let (min_hz, max_hz) = sanitize_if_band(min_hz, max_hz, self.decimated_sample_rate);
        self.if_min_hz = min_hz;
        self.if_max_hz = max_hz;
        self.filter
            .set_fir_bandpass(self.if_min_hz / self.sample_rate, self.if_max_hz / self.sample_rate);
    }

    /// 1ブロックのIQデータ(i8型)を受け取り、オーディオ信号とFFT結果を返す。
    pub fn process_am(&mut self, iq_data: &[i8]) -> js_sys::Array {
        let num_samples = iq_data.len() / 2;

        self.baseband_buffer.clear();
        self.baseband_buffer.reserve(num_samples);

        // ベースバンド処理 & NCO
        for iq in iq_data.chunks_exact(2) {
            let i_val = iq[0] as f32 / 128.0;
            let q_val = iq[1] as f32 / 128.0;
            let sample = Complex::new(i_val, q_val);
            let nco_val = self.nco.step();
            self.baseband_buffer.push(sample * nco_val);
        }

        // デシメーション (LPF + Downsampling)
        let decimated = self.filter.process(&self.baseband_buffer);

        // AM復調
        self.am_buffer.resize(decimated.len(), 0.0);
        self.am_demod.demodulate(&decimated, &mut self.am_buffer);

        // リサンプリング (e.g. 50kHz -> 44.1kHz or 48kHz)
        self.audio_buffer.clear();
        self.audio_buffer.reserve(
            ((self.am_buffer.len() as f32 / self.resampler.source_rate as f32)
                * self.resampler.target_rate as f32
                * 1.5) as usize,
        );
        self.resampler.process(&self.am_buffer, &mut self.audio_buffer);

        // FFT (iq_data の先頭 fft_size * 2 要素を使用)
        self.fft_buffer.fill(-120.0);
        if iq_data.len() >= self.fft.get_n() * 2 {
            self.fft
                .fft(&iq_data[0..self.fft.get_n() * 2], &mut self.fft_buffer);
        }

        let out_array = js_sys::Array::new();
        out_array.push(&js_sys::Float32Array::from(self.audio_buffer.as_slice()));
        out_array.push(&js_sys::Float32Array::from(self.fft_buffer.as_slice()));

        out_array
    }
}
