use num_complex::Complex;

fn design_lowpass_coeffs(num_taps: usize, cutoff_norm: f32) -> Vec<f32> {
    assert!(num_taps > 0, "num_taps must be > 0");
    assert!(
        cutoff_norm > 0.0 && cutoff_norm < 0.5,
        "Invalid cutoff_norm={}",
        cutoff_norm
    );
    let mut coeffs = vec![0.0; num_taps];
    let center = (num_taps - 1) as f32 / 2.0;
    let alpha = 0.54;
    let beta = 0.46;

    for (i, coeff) in coeffs.iter_mut().enumerate() {
        let n = i as f32 - center;
        let sinc = if n == 0.0 {
            2.0 * cutoff_norm
        } else {
            (2.0 * std::f32::consts::PI * cutoff_norm * n).sin() / (std::f32::consts::PI * n)
        };
        let window =
            alpha - beta * (2.0 * std::f32::consts::PI * i as f32 / (num_taps - 1) as f32).cos();
        *coeff = sinc * window;
    }

    // LPFはDCゲイン1に正規化
    let gain = coeffs.iter().sum::<f32>().max(1e-8);
    for c in &mut coeffs {
        *c /= gain;
    }

    coeffs
}

fn design_bandpass_coeffs(num_taps: usize, min_norm: f32, max_norm: f32) -> Vec<f32> {
    assert!(num_taps > 0, "num_taps must be > 0");
    assert!(
        min_norm >= 0.0 && max_norm > min_norm && max_norm < 0.5,
        "Invalid band edges: min_norm={}, max_norm={}",
        min_norm,
        max_norm
    );

    if min_norm == 0.0 {
        return design_lowpass_coeffs(num_taps, max_norm);
    }

    let high = design_lowpass_coeffs(num_taps, max_norm);
    let low = design_lowpass_coeffs(num_taps, min_norm);
    high.iter().zip(low.iter()).map(|(h, l)| h - l).collect()
}

/// 複素ベースバンド用デシメーションフィルタ (簡単なCICやFIR)
/// HackRF（例えば2MHz）から音声レート（たとえば48kHzなど）へとサンプリングレートを落とす。
/// レート変換比（Decimation factor） M とします。
pub enum DecimationFilter {
    Boxcar(BoxcarDecimator),
    Fir(FirDecimator),
}

pub struct BoxcarDecimator {
    factor: usize,
    inv: f32,
    // 入力ストリームに対する間引き位相。チャンク境界を跨いで維持する。
    phase: usize,
    history: Vec<Complex<f32>>,
}

pub struct FirDecimator {
    factor: usize,
    // 入力ストリームに対する間引き位相。チャンク境界を跨いで維持する。
    phase: usize,
    history: Vec<Complex<f32>>,
    coeffs: Vec<f32>,
}

fn update_history(history: &mut [Complex<f32>], input: &[Complex<f32>]) {
    let hist_len = history.len();
    if hist_len == 0 {
        return;
    }

    if input.len() >= hist_len {
        history.copy_from_slice(&input[input.len() - hist_len..]);
    } else {
        // inputが短い場合はシフトして詰める
        let shift = input.len();
        history.copy_within(shift.., 0);
        history[hist_len - shift..].copy_from_slice(input);
    }
}

impl DecimationFilter {
    /// より高精度のカットオフが必要な場合は窓関数付きFIRなどを実装する
    pub fn new_boxcar(factor: usize) -> Self {
        Self::Boxcar(BoxcarDecimator {
            factor,
            inv: 1.0 / factor as f32,
            phase: 0,
            history: vec![Complex::new(0.0, 0.0); factor - 1],
        })
    }

    /// FIRバンドパス（LPF(max) - LPF(min)）を用いたデシメーター
    pub fn new_fir_band(
        factor: usize,
        num_taps: usize,
        min_cutoff_norm: f32,
        max_cutoff_norm: f32,
    ) -> Self {
        Self::Fir(FirDecimator {
            factor,
            phase: 0,
            history: vec![Complex::new(0.0, 0.0); num_taps - 1],
            coeffs: design_bandpass_coeffs(num_taps, min_cutoff_norm, max_cutoff_norm),
        })
    }

    /// より良い遮断特性を持つFIRフィルタを用いたデシメーター
    #[cfg(test)]
    pub fn new_fir(factor: usize, num_taps: usize, cutoff_norm: f32) -> Self {
        Self::new_fir_band(factor, num_taps, 0.0, cutoff_norm)
    }

    /// FIR係数のDCゲイン（係数の総和）を返す
    pub fn coeffs_dc_gain(&self) -> f32 {
        match self {
            Self::Boxcar(_) => 1.0,
            Self::Fir(fir) => fir.coeffs.iter().sum(),
        }
    }

    /// 既存FIR係数を band-pass へ更新する（タップ数は維持）
    pub fn set_fir_bandpass(&mut self, min_cutoff_norm: f32, max_cutoff_norm: f32) {
        match self {
            Self::Fir(fir) => {
                fir.coeffs =
                    design_bandpass_coeffs(fir.coeffs.len(), min_cutoff_norm, max_cutoff_norm);
            }
            Self::Boxcar(boxcar) => {
                let factor = boxcar.factor;
                let phase = boxcar.phase;
                let history = std::mem::take(&mut boxcar.history);
                let coeffs = design_bandpass_coeffs(factor, min_cutoff_norm, max_cutoff_norm);
                *self = Self::Fir(FirDecimator {
                    factor,
                    phase,
                    history,
                    coeffs,
                });
            }
        }
    }

    /// ブロック単位でのフィルタリングとデシメーション
    /// 入力された配列から 1/M に長さを縮小した出力配列を `output` に書き込む
    pub fn process_into(&mut self, input: &[Complex<f32>], output: &mut Vec<Complex<f32>>) {
        match self {
            Self::Boxcar(boxcar) => boxcar.process_into(input, output),
            Self::Fir(fir) => fir.process_into(input, output),
        }
    }

    /// ブロック単位でのフィルタリングとデシメーション
    /// 入力された配列から 1/M に長さを縮小した出力配列を返す
    #[cfg(test)]
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<Complex<f32>> {
        let factor = match self {
            Self::Boxcar(boxcar) => boxcar.factor,
            Self::Fir(fir) => fir.factor,
        };
        let mut output = Vec::with_capacity(input.len() / factor + 1);
        self.process_into(input, &mut output);
        output
    }
}

impl BoxcarDecimator {
    fn process_into(&mut self, input: &[Complex<f32>], output: &mut Vec<Complex<f32>>) {
        output.clear();
        if input.is_empty() {
            return;
        }

        output.reserve(input.len() / self.factor + 1);

        let hist_len = self.history.len();
        let mut current_idx = if self.phase == 0 {
            0
        } else {
            self.factor - self.phase
        };

        while current_idx < input.len() {
            let mut acc = Complex::new(0.0, 0.0);

            if hist_len > 0 && current_idx < hist_len {
                for &v in &self.history[current_idx..hist_len] {
                    acc += v;
                }
                let from_input = current_idx + 1;
                for &v in &input[..from_input] {
                    acc += v;
                }
            } else {
                let input_start = current_idx.saturating_sub(hist_len);
                let input_end = input_start + self.factor;
                for &v in &input[input_start..input_end] {
                    acc += v;
                }
            }

            output.push(acc * self.inv);
            current_idx += self.factor;
        }

        self.phase = (self.phase + input.len()) % self.factor;
        update_history(&mut self.history, input);
    }
}

impl FirDecimator {
    fn process_into(&mut self, input: &[Complex<f32>], output: &mut Vec<Complex<f32>>) {
        output.clear();
        if input.is_empty() {
            return;
        }

        output.reserve(input.len() / self.factor + 1);

        // 前ブロックからの位相ずれを維持し、ブロック境界でも等間隔で間引く。
        let mut current_idx = if self.phase == 0 {
            0
        } else {
            self.factor - self.phase
        };

        while current_idx < input.len() {
            let mut acc = Complex::new(0.0, 0.0);

            for (i, &coeff) in self.coeffs.iter().enumerate() {
                let val = if current_idx >= i {
                    input[current_idx - i]
                } else {
                    let history_back = i - current_idx;
                    self.history[self.history.len() - history_back]
                };
                acc += val * coeff;
            }

            output.push(acc);
            current_idx += self.factor;
        }

        self.phase = (self.phase + input.len()) % self.factor;
        update_history(&mut self.history, input);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_in_chunks(
        filter: &mut DecimationFilter,
        input: &[Complex<f32>],
        chunk_size: usize,
    ) -> Vec<Complex<f32>> {
        let mut output = Vec::new();
        for chunk in input.chunks(chunk_size) {
            output.extend(filter.process(chunk));
        }
        output
    }

    #[test]
    fn test_boxcar_decimation_basic() {
        // factor = 4の確認
        let mut flt = DecimationFilter::new_boxcar(4);

        let input: Vec<Complex<f32>> = vec![
            Complex::new(1.0, 0.0),
            Complex::new(2.0, 0.0),
            Complex::new(3.0, 0.0),
            Complex::new(4.0, 0.0), // Mean = 2.5
            Complex::new(5.0, 0.0),
            Complex::new(6.0, 0.0),
            Complex::new(7.0, 0.0),
            Complex::new(8.0, 0.0), // Mean = 6.5
        ];

        let out = flt.process(&input);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn test_fir_decimation_stopband_attenuation() {
        // ナイキスト周波数より高い信号が正しく減衰（カット）されることを確認する数学的テスト
        // 例: 入力 2MHz, factor=40 (出力 50kHz) のとき
        // 通過帯域: 10kHz未満 (cutoff_norm = 10k/2M = 0.005)
        // ストップバンド: 25kHz以上は確実に落とす
        let mut flt_pass = DecimationFilter::new_fir(40, 61, 0.005);
        let mut flt_stop = DecimationFilter::new_fir(40, 61, 0.005);

        let sample_rate = 2_000_000.0;

        // パスバンド信号 1kHz (通過するはず)
        let pass_freq = 1_000.0;
        let mut input_pass = vec![];
        for i in 0..10_000 {
            let t = i as f32 / sample_rate;
            input_pass.push(Complex::new((2.0 * std::f32::consts::PI * pass_freq * t).cos(), 0.0));
        }

        // ストップバンド信号 100kHz (カットされるはず)
        let stop_freq = 100_000.0;
        let mut input_stop = vec![];
        for i in 0..10_000 {
            let t = i as f32 / sample_rate;
            input_stop.push(Complex::new((2.0 * std::f32::consts::PI * stop_freq * t).cos(), 0.0));
        }

        let out_pass = flt_pass.process(&input_pass);
        assert_eq!(out_pass.len(), 10_000 / 40);

        let out_stop = flt_stop.process(&input_stop);

        // フィルタ安定後のパワーを比較
        let mut pass_power = 0.0;
        for c in &out_pass[10..] {
            pass_power += c.norm_sqr();
        }
        let pass_power = pass_power / (out_pass.len() - 10) as f32;

        let mut stop_power = 0.0;
        for c in &out_stop[10..] {
            stop_power += c.norm_sqr();
        }
        let stop_power = stop_power / (out_stop.len() - 10) as f32;

        // 通過帯と減衰帯で大きな差(-20dB以上など)があること
        assert!(pass_power > 0.4); // 理想的には 0.5 (cos^2の平均)
        assert!(
            stop_power < 0.05,
            "Stopband signal not attenuated sufficiently: {}",
            stop_power
        );
    }

    #[test]
    fn test_fir_decimation_chunk_invariance() {
        let factor = 40;
        let mut flt_whole = DecimationFilter::new_fir(factor, 201, 0.005);
        let mut flt_chunks = DecimationFilter::new_fir(factor, 201, 0.005);

        let sample_rate = 2_000_000.0;
        let len = 131_072 * 3 + 17;
        let mut input = Vec::with_capacity(len);
        for i in 0..len {
            let t = i as f32 / sample_rate;
            let re = 0.7 * (2.0 * std::f32::consts::PI * 3_000.0 * t).cos()
                + 0.2 * (2.0 * std::f32::consts::PI * 12_000.0 * t).cos();
            let im = 0.7 * (2.0 * std::f32::consts::PI * 3_000.0 * t).sin()
                + 0.2 * (2.0 * std::f32::consts::PI * 12_000.0 * t).sin();
            input.push(Complex::new(re, im));
        }

        let out_whole = flt_whole.process(&input);
        let out_chunks = run_in_chunks(&mut flt_chunks, &input, 131_072);

        assert_eq!(out_whole.len(), out_chunks.len());
        let max_err = out_whole
            .iter()
            .zip(out_chunks.iter())
            .map(|(a, b)| (*a - *b).norm())
            .fold(0.0, f32::max);

        assert!(
            max_err < 1e-5,
            "Chunked decimation diverged from whole processing: max_err={}",
            max_err
        );
    }

    #[test]
    fn test_fir_decimation_adjacent_channel_rejection() {
        // 2MHz入力を40分の1に落とすAM想定:
        // 1kHzは通し、隣接帯域側の9kHzは十分抑圧されることを確認する。
        let sample_rate = 2_000_000.0;
        let factor = 40;
        let cutoff_norm = 4_500.0 / sample_rate;
        let mut flt_pass = DecimationFilter::new_fir(factor, 601, cutoff_norm);
        let mut flt_adj = DecimationFilter::new_fir(factor, 601, cutoff_norm);

        let len = 200_000;
        let mut input_pass = Vec::with_capacity(len);
        let mut input_adj = Vec::with_capacity(len);

        for i in 0..len {
            let t = i as f32 / sample_rate;
            input_pass.push(Complex::new(
                (2.0 * std::f32::consts::PI * 1_000.0 * t).cos(),
                (2.0 * std::f32::consts::PI * 1_000.0 * t).sin(),
            ));
            input_adj.push(Complex::new(
                (2.0 * std::f32::consts::PI * 9_000.0 * t).cos(),
                (2.0 * std::f32::consts::PI * 9_000.0 * t).sin(),
            ));
        }

        let out_pass = flt_pass.process(&input_pass);
        let out_adj = flt_adj.process(&input_adj);

        // 立ち上がり過渡を捨ててパワー比較
        let skip = 50usize.min(out_pass.len().saturating_sub(1));
        let pass_power = out_pass[skip..]
            .iter()
            .map(|c| c.norm_sqr())
            .sum::<f32>()
            / (out_pass.len() - skip) as f32;
        let adj_power = out_adj[skip..]
            .iter()
            .map(|c| c.norm_sqr())
            .sum::<f32>()
            / (out_adj.len() - skip) as f32;

        // 少なくとも20dB以上抑圧されること（power比 1/100 未満）
        assert!(
            adj_power < pass_power * 0.01,
            "Adjacent rejection is too weak: pass_power={}, adj_power={}",
            pass_power,
            adj_power
        );
    }

    #[test]
    fn test_fir_bandpass_rejects_dc() {
        let sample_rate = 2_000_000.0;
        let factor = 40;
        let mut flt = DecimationFilter::new_fir_band(
            factor,
            601,
            500.0 / sample_rate,
            5_000.0 / sample_rate,
        );

        let len = 200_000;
        let input: Vec<Complex<f32>> = (0..len).map(|_| Complex::new(1.0, 0.0)).collect();
        let out = flt.process(&input);
        let skip = 50usize.min(out.len().saturating_sub(1));
        let dc_power = out[skip..].iter().map(|c| c.norm_sqr()).sum::<f32>() / (out.len() - skip) as f32;
        assert!(dc_power < 1e-3, "Band-pass should reject DC, got dc_power={}", dc_power);
    }
}
