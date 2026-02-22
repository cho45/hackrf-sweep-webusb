use num_complex::Complex;

/// Number Controlled Oscillator (NCO)
/// 複素ベースバンド変換のための内部発振器
pub struct Nco {
    phase: f32,
    phase_inc: f32,
}

impl Nco {
    pub fn new(freq_hz: f32, sample_rate: f32) -> Self {
        let phase_inc = 2.0 * std::f32::consts::PI * freq_hz / sample_rate;
        Self {
            phase: 0.0,
            phase_inc,
        }
    }

    /// 1サンプル進め、その時点での複素発振値 e^(j * phase) を返す。
    /// これを元の入力信号(Complex)と掛け合わせることで、周波数シフト（ベースバンド変換）を行う。
    pub fn step(&mut self) -> Complex<f32> {
        let val = Complex::new(self.phase.cos(), self.phase.sin());
        self.phase += self.phase_inc;

        // 位相がオーバーフローしないように丸める
        let two_pi = 2.0 * std::f32::consts::PI;
        if self.phase >= two_pi {
            self.phase -= two_pi;
        } else if self.phase < 0.0 {
            self.phase += two_pi;
        }

        val
    }
}

/// AM 復調器（包絡線検波 + キャリア追従 + AGC）
pub struct AMDemodulator {
    carrier_estimate: f32,
    gain: f32,
    carrier_alpha: f32,
    agc_attack_alpha: f32,
    agc_release_alpha: f32,
    target_level: f32,
    max_gain: f32,
    output_clip: f32,
}

impl AMDemodulator {
    pub fn new() -> Self {
        Self {
            carrier_estimate: 0.0,
            gain: 0.0,
            // 50kHz系を想定した緩い追従。搬送波レベルのみ追従し、音声帯域は通す。
            carrier_alpha: 0.0002,
            // ゲインは上げる時を遅く、下げる時を速くして破綻を防ぐ。
            agc_attack_alpha: 0.002,
            agc_release_alpha: 0.02,
            target_level: 0.3,
            max_gain: 50.0,
            output_clip: 0.98,
        }
    }

    /// 複素IQサンプルの配列（ベースバンド）を受け取り、AM包絡線検波（DCカット含む）を行ってオーディオ出力配列に詰める。
    /// - input: デシメーション済み、あるいはベースバンド帯域の IQ 複素データ
    /// - output: 復調された音声データ (f32)
    pub fn demodulate(&mut self, input: &[Complex<f32>], output: &mut [f32]) {
        assert_eq!(input.len(), output.len());

        for (i, sample) in input.iter().enumerate() {
            // 包絡線長
            let env = sample.norm();

            // キャリアレベルのDC追従（搬送波推定）
            self.carrier_estimate += self.carrier_alpha * (env - self.carrier_estimate);

            // AC成分を抽出
            let ac = env - self.carrier_estimate;

            // 搬送波レベルで正規化する AGC
            let desired_gain = if self.carrier_estimate > 1e-4 {
                (self.target_level / self.carrier_estimate).min(self.max_gain)
            } else {
                0.0
            };

            let agc_alpha = if desired_gain > self.gain {
                self.agc_attack_alpha
            } else {
                self.agc_release_alpha
            };
            self.gain += agc_alpha * (desired_gain - self.gain);

            output[i] = (ac * self.gain).clamp(-self.output_clip, self.output_clip);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demod_in_chunks(
        demod: &mut AMDemodulator,
        input: &[Complex<f32>],
        chunk_size: usize,
    ) -> Vec<f32> {
        let mut out = Vec::new();
        for chunk in input.chunks(chunk_size) {
            let mut chunk_out = vec![0.0; chunk.len()];
            demod.demodulate(chunk, &mut chunk_out);
            out.extend_from_slice(&chunk_out);
        }
        out
    }

    #[test]
    fn test_nco_frequency() {
        // NCOが指定した周波数で正しく回転しているかをテスト
        let sample_rate = 1000.0;
        let freq = 250.0; // 1/4 sample rate (90度ずつ進む)
        let mut nco = Nco::new(freq, sample_rate);

        // phase 0
        let c0 = nco.step();
        assert!((c0.re - 1.0).abs() < 1e-6);
        assert!((c0.im - 0.0).abs() < 1e-6);

        // phase PI/2
        let c1 = nco.step();
        assert!(c1.re.abs() < 1e-6);
        assert!((c1.im - 1.0).abs() < 1e-6);

        // phase PI
        let c2 = nco.step();
        assert!((c2.re - (-1.0)).abs() < 1e-6);
        assert!(c2.im.abs() < 1e-6);

        // phase 3PI/2
        let c3 = nco.step();
        assert!(c3.re.abs() < 1e-6);
        assert!((c3.im - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn test_am_demodulation_carrier_only() {
        // 無変調の搬送波（大きさが一定の複素信号）を入力した場合、出力は0(DCカットされるため)に収束するはず
        let mut demod = AMDemodulator::new();
        // 追従安定のため十分な長さで評価
        let input: Vec<Complex<f32>> = (0..120_000).map(|_| Complex::new(5.0, 0.0)).collect();
        let mut output = vec![0.0; input.len()];

        demod.demodulate(&input, &mut output);

        // 十分時間が経った後(最後の方)はほぼ0になっていること
        let tail = &output[100_000..];
        let mean_abs = tail.iter().map(|x| x.abs()).sum::<f32>() / tail.len() as f32;
        assert!(
            mean_abs < 0.02,
            "Output should converge to ~0 for unmodulated carrier, got mean_abs={}",
            mean_abs
        );
    }

    #[test]
    fn test_am_demodulation_with_signal() {
        // 搬送波に低周波の正弦波で変調をかけた信号を入力
        // 包絡線検波とDCカットにより、入力した低周波成分が抽出されることを確認
        let mut demod = AMDemodulator::new();

        let sample_rate = 48_000.0;
        let tone_freq = 1_000.0; // 1kHz トーン
        let carrier_amp = 10.0; // 搬送波振幅
        let mod_index = 0.5; // 変調度 50%

        let mut input = vec![];
        for i in 0..96_000 {
            let t = i as f32 / sample_rate;
            // AM変調波の包絡線: A * (1 + m * cos(2pi * f * t))
            let envelope =
                carrier_amp * (1.0 + mod_index * (2.0 * std::f32::consts::PI * tone_freq * t).cos());

            // ベースバンド複素信号として生成（搬送波はDC 0Hzとするため、位相回転は0）
            input.push(Complex::new(envelope, 0.0));
        }

        let mut output = vec![0.0; input.len()];
        demod.demodulate(&input, &mut output);

        // AGC収束後の1周期で振幅を計測
        let start_idx = 80_000;
        let end_idx = start_idx + 48; // 1kHz @ 48kHz は 48サンプル周期

        let mut max_val = output[start_idx];
        let mut min_val = output[start_idx];
        for &val in &output[start_idx..end_idx] {
            if val > max_val {
                max_val = val;
            }
            if val < min_val {
                min_val = val;
            }
        }

        // AGCが有効なため、出力振幅目標は概ね target_level * mod_index
        let expected_amp = 0.3 * mod_index;
        let actual_amp = (max_val - min_val) / 2.0;

        // 追従型AGCなので 15% 程度の誤差を許容
        let diff = (actual_amp - expected_amp).abs();
        assert!(
            diff < expected_amp * 0.15,
            "AM demodulation failed. Expected amplitude ~{}, got {}, diff={}",
            expected_amp,
            actual_amp,
            diff
        );
    }

    #[test]
    fn test_am_demodulation_chunk_invariance() {
        let mut demod_whole = AMDemodulator::new();
        let mut demod_chunks = AMDemodulator::new();

        let sample_rate = 50_000.0;
        let len = 131_072 * 2 + 513;
        let mut input = Vec::with_capacity(len);

        for i in 0..len {
            let t = i as f32 / sample_rate;
            let m = 1.0 + 0.65 * (2.0 * std::f32::consts::PI * 2300.0 * t).sin();
            let phase = 2.0 * std::f32::consts::PI * 300.0 * t;
            input.push(Complex::new(4.0 * m * phase.cos(), 4.0 * m * phase.sin()));
        }

        let mut out_whole = vec![0.0; input.len()];
        demod_whole.demodulate(&input, &mut out_whole);

        let out_chunks = demod_in_chunks(&mut demod_chunks, &input, 131_072);
        assert_eq!(out_whole.len(), out_chunks.len());

        let max_err = out_whole
            .iter()
            .zip(out_chunks.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);

        assert!(
            max_err < 5e-4,
            "Chunked demodulation diverged from whole processing: max_err={}",
            max_err
        );
    }
}
