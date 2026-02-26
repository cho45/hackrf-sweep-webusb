use num_complex::Complex;

/// FM 復調器（位相差分法 / Discriminator）
///
/// 瞬時周波数偏移を位相差分から求め、MPX信号を復元する。
///
/// # アルゴリズム
/// ```text
/// d[n] = conj(s[n-1]) * s[n]
/// Δφ[n] = atan2(Im(d[n]), Re(d[n]))   // [-π, +π] rad
/// output[n] = Δφ[n] * gain
/// ```
/// ここで `gain = 1 / (2π * max_deviation_hz / sample_rate_hz)`。
///
/// 注: 出力は mono 音声ではなく FM baseband (MPX) を想定する。
pub struct FMDemodulator {
    prev: Complex<f32>,
    /// 出力正規化ゲイン: 1 / (2π * Δf_max / fs)
    gain: f32,
}

impl FMDemodulator {
    /// - `max_deviation_hz`: 最大周波数偏移 [Hz]（WFMなら75_000.0など）
    /// - `sample_rate_hz`: 入力IQのサンプルレート [Hz]（デシメーション後の値）
    pub fn new(max_deviation_hz: f32, sample_rate_hz: f32) -> Self {
        assert!(max_deviation_hz > 0.0, "max_deviation_hz must be > 0");
        assert!(sample_rate_hz > 0.0, "sample_rate_hz must be > 0");
        let gain = sample_rate_hz / (2.0 * std::f32::consts::PI * max_deviation_hz);
        Self {
            prev: Complex::new(1.0, 0.0),
            gain,
        }
    }

    /// 複素IQサンプル列を受け取り、FM復調したMPX信号を output に書き込む。
    pub fn demodulate(&mut self, input: &[Complex<f32>], output: &mut [f32]) {
        assert_eq!(input.len(), output.len());

        for (i, &s) in input.iter().enumerate() {
            let d = self.prev.conj() * s;
            output[i] = d.im.atan2(d.re) * self.gain;
            self.prev = s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_iq_tone(freq_hz: f32, sample_rate: f32, len: usize) -> Vec<Complex<f32>> {
        (0..len)
            .map(|i| {
                let t = i as f32 / sample_rate;
                let phi = 2.0 * std::f32::consts::PI * freq_hz * t;
                Complex::new(phi.cos(), phi.sin())
            })
            .collect()
    }

    fn demod_in_chunks(
        demod: &mut FMDemodulator,
        input: &[Complex<f32>],
        chunk_size: usize,
    ) -> Vec<f32> {
        let mut out = Vec::new();
        for chunk in input.chunks(chunk_size) {
            let mut chunk_out = vec![0.0f32; chunk.len()];
            demod.demodulate(chunk, &mut chunk_out);
            out.extend_from_slice(&chunk_out);
        }
        out
    }

    #[test]
    fn test_fm_constant_deviation() {
        let sample_rate = 200_000.0_f32;
        let max_deviation = 75_000.0_f32;
        let test_freq = 10_000.0_f32;

        let mut demod = FMDemodulator::new(max_deviation, sample_rate);
        let input = make_iq_tone(test_freq, sample_rate, 10_000);
        let mut output = vec![0.0f32; input.len()];
        demod.demodulate(&input, &mut output);

        let expected = test_freq / max_deviation;

        let tail = &output[1..];
        let mean = tail.iter().copied().sum::<f32>() / tail.len() as f32;
        let max_err = tail.iter().map(|&v| (v - expected).abs()).fold(0.0_f32, f32::max);

        assert!(
            max_err < 2e-4,
            "FM constant deviation: expected={}, mean={}, max_err={}",
            expected,
            mean,
            max_err
        );
    }

    #[test]
    fn test_fm_zero_deviation() {
        let sample_rate = 200_000.0_f32;
        let max_deviation = 75_000.0_f32;

        let mut demod = FMDemodulator::new(max_deviation, sample_rate);
        let input: Vec<Complex<f32>> = (0..5_000).map(|_| Complex::new(1.0, 0.0)).collect();
        let mut output = vec![0.0f32; input.len()];
        demod.demodulate(&input, &mut output);

        let tail = &output[1..];
        let max_abs = tail.iter().map(|v| v.abs()).fold(0.0_f32, f32::max);
        assert!(
            max_abs < 1e-6,
            "FM zero deviation: output should be ~0, got max_abs={}",
            max_abs
        );
    }

    #[test]
    fn test_fm_chunk_invariance() {
        let sample_rate = 200_000.0_f32;
        let max_deviation = 75_000.0_f32;

        let mut demod_whole = FMDemodulator::new(max_deviation, sample_rate);
        let mut demod_chunks = FMDemodulator::new(max_deviation, sample_rate);

        let len = 131_072 * 2 + 513;
        let mut input = Vec::with_capacity(len);
        for i in 0..len {
            let t = i as f32 / sample_rate;
            let phi = 2.0 * std::f32::consts::PI
                * (50_000.0 * t
                    + (max_deviation / 1_000.0)
                        * (2.0 * std::f32::consts::PI * 1_000.0 * t).sin());
            input.push(Complex::new(phi.cos(), phi.sin()));
        }

        let mut out_whole = vec![0.0f32; len];
        demod_whole.demodulate(&input, &mut out_whole);

        let out_chunks = demod_in_chunks(&mut demod_chunks, &input, 131_072);
        assert_eq!(out_whole.len(), out_chunks.len());

        let max_err = out_whole
            .iter()
            .zip(out_chunks.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);

        assert!(
            max_err < 1e-6,
            "Chunked FM demodulation diverged: max_err={}",
            max_err
        );
    }

    #[test]
    fn test_fm_negative_deviation() {
        let sample_rate = 200_000.0_f32;
        let max_deviation = 75_000.0_f32;
        let test_freq = -10_000.0_f32;

        let mut demod = FMDemodulator::new(max_deviation, sample_rate);
        let input = make_iq_tone(test_freq, sample_rate, 5_000);
        let mut output = vec![0.0f32; input.len()];
        demod.demodulate(&input, &mut output);

        let expected = test_freq / max_deviation;
        let tail = &output[1..];
        let max_err = tail.iter().map(|&v| (v - expected).abs()).fold(0.0_f32, f32::max);

        assert!(
            max_err < 2e-4,
            "FM negative deviation: expected={}, max_err={}",
            expected,
            max_err
        );
    }
}
