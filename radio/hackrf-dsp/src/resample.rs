pub struct Resampler {
    pub source_rate: u32,
    pub target_rate: u32,
    step: f64,
    phase: f64,
    num_phases: usize,
    taps_per_phase: usize,
    coeffs: Vec<Vec<f32>>,
    history: Vec<f32>,
}

impl Resampler {
    pub fn new(source_rate: u32, target_rate: u32) -> Self {
        assert!(source_rate > 0, "source_rate must be > 0");
        assert!(target_rate > 0, "target_rate must be > 0");

        // 入力時間軸上での出力サンプル刻み
        let step = source_rate as f64 / target_rate as f64;

        // 低レイテンシと帯域特性のバランス点
        let num_phases = 256;
        let taps_per_phase = 17; // 奇数タップ（線形位相）
        let mut coeffs = vec![vec![0.0; taps_per_phase]; num_phases];

        // source 側正規化周波数（Nyquist=0.5）
        let cutoff = 0.5f64 * (target_rate as f64 / source_rate as f64).min(1.0) * 0.95;
        let center = (taps_per_phase - 1) as f64 / 2.0;

        for (p, phase_coeffs) in coeffs.iter_mut().enumerate() {
            let frac = p as f64 / num_phases as f64;
            let mut sum = 0.0f64;

            for (i, coeff) in phase_coeffs.iter_mut().enumerate() {
                let x = i as f64 - center - frac;
                let sinc_arg = 2.0 * cutoff * x;
                let sinc = if sinc_arg.abs() < 1e-12 {
                    1.0
                } else {
                    let pix = std::f64::consts::PI * sinc_arg;
                    pix.sin() / pix
                };

                // Blackman window
                let w = 0.42
                    - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (taps_per_phase - 1) as f64).cos()
                    + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / (taps_per_phase - 1) as f64).cos();

                let h = 2.0 * cutoff * sinc * w;
                *coeff = h as f32;
                sum += h;
            }

            // DCゲインを1に正規化
            let inv_sum = 1.0f64 / sum;
            for coeff in phase_coeffs {
                *coeff = (*coeff as f64 * inv_sum) as f32;
            }
        }

        Self {
            source_rate,
            target_rate,
            step,
            phase: 0.0,
            num_phases,
            taps_per_phase,
            coeffs,
            history: vec![0.0; taps_per_phase - 1],
        }
    }

    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }

        let prefix_len = self.history.len();

        // 履歴 + 入力
        let mut buffer = Vec::with_capacity(prefix_len + input.len());
        buffer.extend_from_slice(&self.history);
        buffer.extend_from_slice(input);

        let center = (self.taps_per_phase as isize - 1) / 2;
        // 未来サンプルが必要な範囲（phase >= len - center）は次チャンクへ持ち越す。
        let safe_limit = input.len() as f64 - center as f64;
        while self.phase < safe_limit {
            let base = self.phase.floor() as isize;
            let frac = self.phase - base as f64;

            let mut phase_idx = (frac * self.num_phases as f64).floor() as usize;
            if phase_idx >= self.num_phases {
                phase_idx = self.num_phases - 1;
            }

            let coeffs = &self.coeffs[phase_idx];
            let start = base - center;

            let mut sum = 0.0f32;
            for (tap, &h) in coeffs.iter().enumerate() {
                let src_idx = start + tap as isize;
                let buf_idx = (src_idx + prefix_len as isize) as usize;
                sum += buffer[buf_idx] * h;
            }

            output.push(sum);
            self.phase += self.step;
        }

        self.phase -= input.len() as f64;

        if prefix_len == 0 {
            return;
        }

        if input.len() >= prefix_len {
            self.history
                .copy_from_slice(&input[input.len() - prefix_len..]);
        } else {
            self.history.copy_within(input.len().., 0);
            self.history[prefix_len - input.len()..].copy_from_slice(input);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;
    use rustfft::{num_complex::Complex, FftPlanner};

    fn dominant_frequency(samples: &[f32], sample_rate: u32) -> f32 {
        let n = samples.len();
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(n);

        let mut complex_buffer: Vec<Complex<f32>> =
            samples.iter().map(|&val| Complex::new(val, 0.0)).collect();
        fft.process(&mut complex_buffer);

        let mut max_magnitude = 0.0;
        let mut peak_index = 0;
        for (i, val) in complex_buffer.iter().enumerate().take(n / 2).skip(1) {
            let magnitude = val.norm();
            if magnitude > max_magnitude {
                max_magnitude = magnitude;
                peak_index = i;
            }
        }

        (peak_index as f32 * sample_rate as f32) / n as f32
    }

    // リサンプリング前後で特定の正弦波の周波数が維持されているかをテストする関数
    fn test_resampling_sine_wave(
        source_rate: u32,
        target_rate: u32,
        test_freq: f32,
        duration_sec: f32,
    ) {
        let num_samples_in = (source_rate as f32 * duration_sec).ceil() as usize;

        // 指定周波数の正弦波を生成 (入力)
        let mut input = Vec::with_capacity(num_samples_in);
        for i in 0..num_samples_in {
            let t = i as f32 / source_rate as f32;
            let val = (2.0 * PI * test_freq * t).sin();
            input.push(val);
        }

        // リサンプラに通す
        let mut resampler = Resampler::new(source_rate, target_rate);
        let mut output = Vec::new();
        resampler.process(&input, &mut output);

        // 出力のFFTを行い、ピーク周波数を探す
        let output_len = output.len();
        assert!(output_len > 0, "Output should not be empty");

        let detected_freq = dominant_frequency(&output, target_rate);

        // 分解能の許容範囲 (bin size) を計算
        let freq_resolution = target_rate as f32 / output_len as f32;
        let tolerance = freq_resolution; // ±1bin 程度の誤差を許容

        assert!(
            (detected_freq - test_freq).abs() <= tolerance,
            "Frequency mismatch! Expected ~{} Hz, detected ~{} Hz (Source rate: {}, Target rate: {}, Resolution: {})",
            test_freq,
            detected_freq,
            source_rate,
            target_rate,
            freq_resolution
        );
    }

    #[test]
    fn test_downsampling_preserves_frequency() {
        // 50kHz から 48kHz へのダウンサンプリング (HackRF側のAM復調後などで想定されるケース)
        test_resampling_sine_wave(50_000, 48_000, 1_000.0, 0.5);
    }

    #[test]
    fn test_upsampling_preserves_frequency() {
        // 44.1kHz から 48kHz へのアップサンプリング
        test_resampling_sine_wave(44_100, 48_000, 4_000.0, 0.5);
    }

    #[test]
    fn test_continuous_processing() {
        // 連続的にバッファを渡した場合に、履歴(history)と位相が正しく接続されるかを検証
        let source_rate = 10_000;
        let target_rate = 8_000;
        let mut resampler_chunks = Resampler::new(source_rate, target_rate);
        let mut resampler_whole = Resampler::new(source_rate, target_rate);

        let input: Vec<f32> = (0..4_000)
            .map(|i| {
                let t = i as f32 / source_rate as f32;
                (2.0 * PI * 410.0 * t).sin() + 0.3 * (2.0 * PI * 1200.0 * t).sin()
            })
            .collect();

        // チャンクに分割して処理
        let mut out_chunks = Vec::new();
        for chunk in input.chunks(137) {
            resampler_chunks.process(chunk, &mut out_chunks);
        }

        // 一括で処理
        let mut out_whole = Vec::new();
        resampler_whole.process(&input, &mut out_whole);

        assert!((out_chunks.len() as isize - out_whole.len() as isize).abs() <= 1);

        let min_len = out_chunks.len().min(out_whole.len());
        let rmse = (out_chunks[..min_len]
            .iter()
            .zip(out_whole[..min_len].iter())
            .map(|(a, b)| {
                let d = a - b;
                d * d
            })
            .sum::<f32>()
            / min_len as f32)
            .sqrt();

        assert!(rmse < 1e-4, "Chunked output diverged from whole output: rmse={}", rmse);
    }
}
