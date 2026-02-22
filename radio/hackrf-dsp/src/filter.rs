use num_complex::Complex;

/// 複素ベースバンド用デシメーションフィルタ (簡単なCICやFIR)
/// HackRF（例えば2MHz）から音声レート（たとえば48kHzなど）へとサンプリングレートを落とす。
/// レート変換比（Decimation factor） M とします。
pub struct DecimationFilter {
    factor: usize,
    // 入力ストリームに対する間引き位相。チャンク境界を跨いで維持する。
    phase: usize,
    history: Vec<Complex<f32>>,
    coeffs: Vec<f32>,
}

impl DecimationFilter {
    /// より高精度のカットオフが必要な場合は窓関数付きFIRなどを実装する
    #[allow(dead_code)]
    pub fn new_boxcar(factor: usize) -> Self {
        // 単純移動平均の係数は全て 1/M
        let coeffs = vec![1.0 / (factor as f32); factor];
        Self {
            factor,
            phase: 0,
            history: vec![Complex::new(0.0, 0.0); factor - 1],
            coeffs,
        }
    }

    /// より良い遮断特性を持つFIRフィルタを用いたデシメーター
    pub fn new_fir(factor: usize, num_taps: usize, cutoff_norm: f32) -> Self {
        let mut coeffs = vec![0.0; num_taps];
        // Sinc + Hamming window によるローパスフィルタ設計
        let mut sum = 0.0;
        let alpha = 0.54;
        let beta = 0.46;
        let center = (num_taps - 1) as f32 / 2.0;

        for (i, coeff) in coeffs.iter_mut().enumerate() {
            let n = i as f32 - center;
            // ideal Sinc
            let sinc = if n == 0.0 {
                2.0 * cutoff_norm
            } else {
                (2.0 * std::f32::consts::PI * cutoff_norm * n).sin() / (std::f32::consts::PI * n)
            };
            // Hamming window
            let window = alpha - beta * (2.0 * std::f32::consts::PI * i as f32 / (num_taps - 1) as f32).cos();
            *coeff = sinc * window;
            sum += *coeff;
        }

        // ゲインを1に正規化
        for c in &mut coeffs {
            *c /= sum;
        }

        Self {
            factor,
            phase: 0,
            history: vec![Complex::new(0.0, 0.0); num_taps - 1],
            coeffs,
        }
    }

    /// ブロック単位でのフィルタリングとデシメーション
    /// 入力された配列から 1/M に長さを縮小した出力配列を返す
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<Complex<f32>> {
        if input.is_empty() {
            return Vec::new();
        }

        let mut output = Vec::with_capacity(input.len() / self.factor + 1);

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

        // history の更新 (次回の入力を考慮して末尾タップ-1個を保存)
        let hist_len = self.history.len();
        if hist_len == 0 {
            return output;
        }

        if input.len() >= hist_len {
            self.history.copy_from_slice(&input[input.len() - hist_len..]);
        } else {
            // inputが短い場合はシフトして詰める
            let shift = input.len();
            self.history.copy_within(shift.., 0);
            self.history[hist_len - shift..].copy_from_slice(input);
        }

        output
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
}
