pub struct Resampler {
    pub source_rate: u32,
    pub target_rate: u32,
    step: f64,
    phase: f64,
    num_phases: usize,
    taps_per_phase: usize,
    coeffs: Vec<Vec<f32>>,
    history: Vec<f32>,
    scratch: Vec<f32>,
}

impl Resampler {
    /// `cutoff_hz` を指定した場合、リサンプラ内LPFのカットオフを明示できる。
    /// 指定しない場合は従来どおり target_rate ベースの自動値を使う。
    pub fn new_with_cutoff(source_rate: u32, target_rate: u32, cutoff_hz: Option<f32>) -> Self {
        assert!(source_rate > 0, "source_rate must be > 0");
        assert!(target_rate > 0, "target_rate must be > 0");

        // 入力時間軸上での出力サンプル刻み
        let step = source_rate as f64 / target_rate as f64;

        let num_phases = 256;
        // step ≈ 1 で 17 タップ、step ≈ 4.2 で ~85 タップ。
        let taps_per_phase = {
            let raw = (step.ceil() as usize * 17).max(17);
            raw | 1 // 奇数保証
        };
        let mut coeffs = vec![vec![0.0; taps_per_phase]; num_phases];

        // source 側正規化周波数（Nyquist=0.5）
        // - 指定なし: target_rate に合わせた従来値
        // - 指定あり: min(source/2, target/2, cutoff_hz) にクランプして利用
        let default_cutoff_hz = 0.5f64 * target_rate as f64 * 0.95;
        let max_cutoff_hz = 0.49f64 * (source_rate.min(target_rate) as f64);
        let cutoff_hz = cutoff_hz
            .map(|v| v as f64)
            .unwrap_or(default_cutoff_hz)
            .min(max_cutoff_hz)
            .max(1.0);
        let cutoff = cutoff_hz / source_rate as f64;
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
            scratch: Vec::new(),
        }
    }

    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }

        let prefix_len = self.history.len();

        // 履歴 + 入力
        self.scratch.clear();
        self.scratch.reserve(prefix_len + input.len());
        self.scratch.extend_from_slice(&self.history);
        self.scratch.extend_from_slice(input);
        let buffer = &self.scratch;

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
        let mut resampler = Resampler::new_with_cutoff(source_rate, target_rate, None);
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
        let mut resampler_chunks = Resampler::new_with_cutoff(source_rate, target_rate, None);
        let mut resampler_whole = Resampler::new_with_cutoff(source_rate, target_rate, None);

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

    #[test]
    fn test_high_ratio_downsampling_preserves_frequency() {
        // WFM 復調後の 200kHz → 48kHz ダウンサンプリング (step ≈ 4.17)
        test_resampling_sine_wave(200_000, 48_000, 1_000.0, 0.5);
    }

    #[test]
    fn test_high_ratio_downsampling_stopband() {
        // 200kHz → 48kHz ダウンサンプリング時、ナイキスト (24kHz) 超の信号が
        // 十分に減衰されることを確認する。
        let source_rate = 200_000u32;
        let target_rate = 48_000u32;
        let mut resampler = Resampler::new_with_cutoff(source_rate, target_rate, None);

        // 30kHz (ナイキスト超) の正弦波を入力
        let len = 50_000;
        let input: Vec<f32> = (0..len)
            .map(|i| {
                let t = i as f32 / source_rate as f32;
                (2.0 * PI * 30_000.0 * t).sin()
            })
            .collect();

        let mut output = Vec::new();
        resampler.process(&input, &mut output);

        // FIR 過渡を除いてパワーを計算
        let skip = 100.min(output.len().saturating_sub(1));
        let power = output[skip..]
            .iter()
            .map(|v| v * v)
            .sum::<f32>()
            / (output.len() - skip) as f32;

        // 入力パワーは 0.5 (sin^2平均)。少なくとも -30dB (0.001) 以下に減衰。
        assert!(
            power < 0.001,
            "Stopband signal not attenuated: power={}",
            power
        );
    }

    #[test]
    fn test_long_run_output_count_tracks_ratio_50k_to_48k() {
        let source_rate = 50_000u32;
        let target_rate = 48_000u32;
        let mut resampler = Resampler::new_with_cutoff(source_rate, target_rate, None);

        let total_input = 500_000usize; // 10秒相当
        let input: Vec<f32> = (0..total_input)
            .map(|i| {
                let t = i as f32 / source_rate as f32;
                (2.0 * PI * 1_200.0 * t).sin() + 0.2 * (2.0 * PI * 7_500.0 * t).sin()
            })
            .collect();

        let chunk_pattern = [127usize, 509, 1021, 4093];
        let mut pos = 0usize;
        let mut chunk_idx = 0usize;
        let mut output = Vec::new();
        while pos < input.len() {
            let chunk_len = chunk_pattern[chunk_idx % chunk_pattern.len()];
            let end = (pos + chunk_len).min(input.len());
            resampler.process(&input[pos..end], &mut output);
            pos = end;
            chunk_idx += 1;
        }

        let expected = ((total_input as f64) * (target_rate as f64) / (source_rate as f64)).round() as isize;
        let actual = output.len() as isize;
        let err = (actual - expected).abs();

        // 有限長処理のため端の遅延分は残るが、誤差はフィルタ長オーダーに収まるべき。
        let tolerance = resampler.taps_per_phase as isize + 4;
        assert!(
            err <= tolerance,
            "Long-run count drift too large: actual={} expected={} err={} tol={}",
            actual,
            expected,
            err,
            tolerance
        );
    }

    #[test]
    fn test_long_run_output_count_tracks_ratio_200k_to_48k() {
        let source_rate = 200_000u32;
        let target_rate = 48_000u32;
        let mut resampler = Resampler::new_with_cutoff(source_rate, target_rate, None);

        let total_input = 2_000_000usize; // 10秒相当
        let input: Vec<f32> = (0..total_input)
            .map(|i| {
                let t = i as f32 / source_rate as f32;
                (2.0 * PI * 900.0 * t).sin() + 0.2 * (2.0 * PI * 18_000.0 * t).sin()
            })
            .collect();

        let chunk_pattern = [113usize, 701, 4096, 8191];
        let mut pos = 0usize;
        let mut chunk_idx = 0usize;
        let mut output = Vec::new();
        while pos < input.len() {
            let chunk_len = chunk_pattern[chunk_idx % chunk_pattern.len()];
            let end = (pos + chunk_len).min(input.len());
            resampler.process(&input[pos..end], &mut output);
            pos = end;
            chunk_idx += 1;
        }

        let expected = ((total_input as f64) * (target_rate as f64) / (source_rate as f64)).round() as isize;
        let actual = output.len() as isize;
        let err = (actual - expected).abs();

        let tolerance = resampler.taps_per_phase as isize + 4;
        assert!(
            err <= tolerance,
            "Long-run count drift too large: actual={} expected={} err={} tol={}",
            actual,
            expected,
            err,
            tolerance
        );
    }

    #[test]
    fn test_custom_cutoff_reduces_high_audio_band() {
        let source_rate = 50_000u32;
        let target_rate = 48_000u32;
        let tone_hz = 10_000.0f32;
        let len = 120_000usize;

        let input: Vec<f32> = (0..len)
            .map(|i| {
                let t = i as f32 / source_rate as f32;
                (2.0 * PI * tone_hz * t).sin()
            })
            .collect();

        let mut default_rs = Resampler::new_with_cutoff(source_rate, target_rate, None);
        let mut low_cut_rs = Resampler::new_with_cutoff(source_rate, target_rate, Some(5_000.0));
        let mut out_default = Vec::new();
        let mut out_low_cut = Vec::new();
        default_rs.process(&input, &mut out_default);
        low_cut_rs.process(&input, &mut out_low_cut);

        let skip_default = out_default.len().min(2_000);
        let skip_low = out_low_cut.len().min(2_000);
        let rms_default = (out_default[skip_default..]
            .iter()
            .map(|v| v * v)
            .sum::<f32>()
            / (out_default.len() - skip_default).max(1) as f32)
            .sqrt();
        let rms_low_cut = (out_low_cut[skip_low..]
            .iter()
            .map(|v| v * v)
            .sum::<f32>()
            / (out_low_cut.len() - skip_low).max(1) as f32)
            .sqrt();

        assert!(
            rms_low_cut < rms_default * 0.3,
            "Custom cutoff did not attenuate high tone enough: default_rms={} low_cut_rms={}",
            rms_default,
            rms_low_cut
        );
    }
}
