use num_complex::Complex;

/// FM 復調器（位相差分法 / Discriminator）
///
/// 瞬時周波数偏移を位相差分から求め、音声信号を復元する。
///
/// # アルゴリズム
/// ```text
/// d[n] = conj(s[n-1]) * s[n]
/// Δφ[n] = atan2(Im(d[n]), Re(d[n]))   // [-π, +π] rad
/// output[n] = Δφ[n] * gain
/// ```
/// ここで `gain = 1 / (2π * max_deviation_hz / sample_rate_hz)`
/// とすることで出力を [-1.0, +1.0] に正規化する（最大偏移時に ±1.0）。
pub struct FMDemodulator {
    prev: Complex<f32>,
    /// 出力正規化ゲイン: 1 / (2π * Δf_max / fs)
    gain: f32,
}

impl FMDemodulator {
    /// - `max_deviation_hz`: 最大周波数偏移 [Hz]（WFMなら75_000.0、NFMなら2_500.0など）
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

    /// 複素IQサンプル列を受け取り、FM復調した音声を output に書き込む。
    /// - input: デシメーション済み複素ベースバンド IQ
    /// - output: 復調音声 f32（最大偏移で概ね ±1.0）
    pub fn demodulate(&mut self, input: &[Complex<f32>], output: &mut [f32]) {
        assert_eq!(input.len(), output.len());

        for (i, &s) in input.iter().enumerate() {
            // conj(prev) * s = |prev||s| * e^(j*Δφ)
            // normが0の場合は無音とする（除算回避）
            let d = self.prev.conj() * s;
            // atan2 は O(1) で [-π, +π] に収まるため位相アンラップ不要
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

    /// 純粋な複素正弦波（一定周波数偏移）を入力すると、
    /// 出力が一定値（= 偏移量を正規化した値）に収束することを確認する。
    ///
    /// IQ が e^(j*2π*f*t) の場合、位相差分 Δφ = 2π*f/fs [rad/sample] = 一定。
    /// ゆえに出力 = f / max_deviation（最大偏移で ±1.0）。
    #[test]
    fn test_fm_constant_deviation() {
        let sample_rate = 200_000.0_f32;
        let max_deviation = 75_000.0_f32;
        let test_freq = 10_000.0_f32; // 10kHz 偏移 = max_dev の 2/15

        let mut demod = FMDemodulator::new(max_deviation, sample_rate);
        let input = make_iq_tone(test_freq, sample_rate, 10_000);
        let mut output = vec![0.0f32; input.len()];
        demod.demodulate(&input, &mut output);

        // 期待値: test_freq / max_deviation
        let expected = test_freq / max_deviation;

        // 最初の1サンプルは prev=1+0j の初期値による誤差があるため、2サンプル目以降で評価
        let tail = &output[1..];
        let mean = tail.iter().copied().sum::<f32>() / tail.len() as f32;
        let max_err = tail.iter().map(|&v| (v - expected).abs()).fold(0.0_f32, f32::max);

        // f32 の cos/sin 生成誤差と atan2 精度の組み合わせで ~1e-4 程度の誤差が生じる。
        // 音声用途では 0.15% の誤差は無視できるため 2e-4 を許容値とする。
        assert!(
            max_err < 2e-4,
            "FM constant deviation: expected={}, mean={}, max_err={}",
            expected,
            mean,
            max_err
        );
    }

    /// 0偏移（単純なDC複素信号）を入力すると出力は0になる。
    /// ただし初期サンプルは prev の影響を受けるため、2サンプル目以降で判断。
    #[test]
    fn test_fm_zero_deviation() {
        let sample_rate = 200_000.0_f32;
        let max_deviation = 75_000.0_f32;

        let mut demod = FMDemodulator::new(max_deviation, sample_rate);
        // DC信号（位相変化なし）
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

    /// チャンク分割の前後で出力が一致することを確認（ステート管理の正確性）。
    #[test]
    fn test_fm_chunk_invariance() {
        let sample_rate = 200_000.0_f32;
        let max_deviation = 75_000.0_f32;

        let mut demod_whole = FMDemodulator::new(max_deviation, sample_rate);
        let mut demod_chunks = FMDemodulator::new(max_deviation, sample_rate);

        // 複数の偏移周波数を混ぜた複雑な信号
        let len = 131_072 * 2 + 513;
        let mut input = Vec::with_capacity(len);
        for i in 0..len {
            let t = i as f32 / sample_rate;
            // FMの瞬時位相: 搬送波 + 正弦波変調
            let phi = 2.0 * std::f32::consts::PI
                * (50_000.0 * t + (max_deviation / 1_000.0) * (2.0 * std::f32::consts::PI * 1_000.0 * t).sin());
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

    /// マイナス周波数偏移（負方向の螺旋）でも正しく負値を出力する。
    #[test]
    fn test_fm_negative_deviation() {
        let sample_rate = 200_000.0_f32;
        let max_deviation = 75_000.0_f32;
        let test_freq = -10_000.0_f32; // 負の偏移

        let mut demod = FMDemodulator::new(max_deviation, sample_rate);
        let input = make_iq_tone(test_freq, sample_rate, 5_000);
        let mut output = vec![0.0f32; input.len()];
        demod.demodulate(&input, &mut output);

        let expected = test_freq / max_deviation; // 負値
        let tail = &output[1..];
        let max_err = tail.iter().map(|&v| (v - expected).abs()).fold(0.0_f32, f32::max);

        // f32 精度限界（~1e-4）を考慮した許容値
        assert!(
            max_err < 2e-4,
            "FM negative deviation: expected={}, max_err={}",
            expected,
            max_err
        );
    }
}
