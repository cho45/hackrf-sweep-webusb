use num_complex::Complex;

/// 複素IQ向けの急峻DCノッチ（2次IIR）
///
/// 伝達関数:
/// H(z) = (1 - 2z^-1 + z^-2) / (1 - 2r z^-1 + r^2 z^-2)
///
/// - z=1 に零点を置くため、DCを強く抑圧する
/// - r を 1 に近づけるほどノッチは急峻になる
struct DcNotch2 {
    r: f32,
    x1: Complex<f32>,
    x2: Complex<f32>,
    y1: Complex<f32>,
    y2: Complex<f32>,
}

impl DcNotch2 {
    fn new(r: f32) -> Self {
        Self {
            r,
            x1: Complex::new(0.0, 0.0),
            x2: Complex::new(0.0, 0.0),
            y1: Complex::new(0.0, 0.0),
            y2: Complex::new(0.0, 0.0),
        }
    }

    fn process(&mut self, sample: Complex<f32>) -> Complex<f32> {
        let r2 = self.r * self.r;
        let y = sample - self.x1 * 2.0 + self.x2 + self.y1 * (2.0 * self.r) - self.y2 * r2;

        self.x2 = self.x1;
        self.x1 = sample;
        self.y2 = self.y1;
        self.y1 = y;

        y
    }
}

/// 複素IQ向けDCキャンセラ（2次ノッチを2段カスケード = 実質4次IIR）
pub struct DcCanceller {
    stage1: DcNotch2,
    stage2: DcNotch2,
}

impl DcCanceller {
    pub fn new(sample_rate_hz: f32, q: f32) -> Self {
        assert!(sample_rate_hz > 0.0, "sample_rate_hz must be > 0");
        assert!(q > 1.0, "q must be > 1");

        // Qを狭帯域化係数として扱い、等価ノッチ幅を sample_rate / Q としてrへ変換する。
        let notch_bw_hz = sample_rate_hz / q;
        let r = (-2.0 * std::f32::consts::PI * notch_bw_hz / sample_rate_hz).exp();

        Self {
            stage1: DcNotch2::new(r),
            stage2: DcNotch2::new(r),
        }
    }

    pub fn process(&mut self, sample: Complex<f32>) -> Complex<f32> {
        let y1 = self.stage1.process(sample);
        self.stage2.process(y1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_whole(canceller: &mut DcCanceller, input: &[Complex<f32>]) -> Vec<Complex<f32>> {
        input.iter().map(|&x| canceller.process(x)).collect()
    }

    fn run_in_chunks(
        canceller: &mut DcCanceller,
        input: &[Complex<f32>],
        chunk_size: usize,
    ) -> Vec<Complex<f32>> {
        let mut out = Vec::with_capacity(input.len());
        for chunk in input.chunks(chunk_size) {
            for &x in chunk {
                out.push(canceller.process(x));
            }
        }
        out
    }

    #[test]
    #[should_panic(expected = "sample_rate_hz must be > 0")]
    fn test_new_rejects_zero_sample_rate() {
        let _ = DcCanceller::new(0.0, 20_000.0);
    }

    #[test]
    #[should_panic(expected = "q must be > 1")]
    fn test_new_rejects_invalid_q() {
        let _ = DcCanceller::new(2_000_000.0, 1.0);
    }

    #[test]
    fn test_constant_dc_converges_to_zero() {
        let mut canceller = DcCanceller::new(2_000_000.0, 20_000.0);
        let dc = Complex::new(0.35, -0.17);
        let input = vec![dc; 120_000];
        let out = run_whole(&mut canceller, &input);

        // 収束後テール区間の平均絶対値を評価
        let tail = &out[90_000..];
        let mean_norm = tail.iter().map(|v| v.norm()).sum::<f32>() / tail.len() as f32;
        assert!(
            mean_norm < 1e-6,
            "DC should be canceled; mean tail norm too large: {}",
            mean_norm
        );
    }

    #[test]
    fn test_ac_component_is_preserved_while_dc_removed() {
        let sample_rate = 2_000_000.0;
        let tone_hz = 8_000.0;
        let mut canceller = DcCanceller::new(sample_rate, 20_000.0);

        let len = 300_000usize;
        let dc = Complex::new(0.4, -0.25);
        let mut input = Vec::with_capacity(len);
        let mut ideal_ac = Vec::with_capacity(len);

        for i in 0..len {
            let t = i as f32 / sample_rate;
            let phase = 2.0 * std::f32::consts::PI * tone_hz * t;
            let ac = Complex::new(phase.cos(), phase.sin());
            ideal_ac.push(ac);
            input.push(ac + dc);
        }

        let out = run_whole(&mut canceller, &input);

        // 初期過渡を除去して評価
        let skip = 160_000usize;
        let out_tail = &out[skip..];
        let ac_tail = &ideal_ac[skip..];

        let mean_re = out_tail.iter().map(|v| v.re).sum::<f32>() / out_tail.len() as f32;
        let mean_im = out_tail.iter().map(|v| v.im).sum::<f32>() / out_tail.len() as f32;
        assert!(
            mean_re.abs() < 5e-3 && mean_im.abs() < 5e-3,
            "Output DC residual too large: mean_re={}, mean_im={}",
            mean_re,
            mean_im
        );

        let rms_out = (out_tail.iter().map(|v| v.norm_sqr()).sum::<f32>() / out_tail.len() as f32).sqrt();
        let rms_ac = (ac_tail.iter().map(|v| v.norm_sqr()).sum::<f32>() / ac_tail.len() as f32).sqrt();
        let ratio = rms_out / rms_ac;
        assert!(
            (0.995..=1.005).contains(&ratio),
            "AC gain out of range: ratio={}",
            ratio
        );
    }

    #[test]
    fn test_near_dc_is_strongly_attenuated() {
        let sample_rate = 2_000_000.0;
        let mut canceller = DcCanceller::new(sample_rate, 20_000.0);

        let len = 400_000usize;
        let tone_hz = 30.0;
        let mut input = Vec::with_capacity(len);
        for i in 0..len {
            let t = i as f32 / sample_rate;
            let phase = 2.0 * std::f32::consts::PI * tone_hz * t;
            input.push(Complex::new(phase.cos(), phase.sin()));
        }

        let out = run_whole(&mut canceller, &input);
        let skip = 100_000usize;
        let out_rms = (out[skip..].iter().map(|v| v.norm_sqr()).sum::<f32>() / (len - skip) as f32).sqrt();

        // 30Hzはノッチ近傍として十分抑圧されること（-20dB以下）
        assert!(
            out_rms < 0.1,
            "Near-DC tone attenuation too weak: out_rms={}",
            out_rms
        );
    }

    #[test]
    fn test_chunk_invariance() {
        let sample_rate = 2_000_000.0;
        let mut whole = DcCanceller::new(sample_rate, 20_000.0);
        let mut chunked = DcCanceller::new(sample_rate, 20_000.0);

        let len = 131_072 * 3 + 29;
        let mut input = Vec::with_capacity(len);
        for i in 0..len {
            let t = i as f32 / sample_rate;
            let re = 0.7
                + 0.8 * (2.0 * std::f32::consts::PI * 1_700.0 * t).cos()
                + 0.2 * (2.0 * std::f32::consts::PI * 13_000.0 * t).cos();
            let im = -0.5
                + 0.8 * (2.0 * std::f32::consts::PI * 1_700.0 * t).sin()
                + 0.2 * (2.0 * std::f32::consts::PI * 13_000.0 * t).sin();
            input.push(Complex::new(re, im));
        }

        let out_whole = run_whole(&mut whole, &input);
        let out_chunked = run_in_chunks(&mut chunked, &input, 131_072);
        assert_eq!(out_whole.len(), out_chunked.len());

        let max_err = out_whole
            .iter()
            .zip(out_chunked.iter())
            .map(|(a, b)| (*a - *b).norm())
            .fold(0.0, f32::max);
        assert!(
            max_err < 1e-7,
            "Chunked processing diverged from whole processing: max_err={}",
            max_err
        );
    }

    #[test]
    fn test_extreme_q_is_finite_and_stable() {
        let mut canceller = DcCanceller::new(2_000_000.0, 1_000_000_000.0);
        let mut max_norm = 0.0f32;
        for i in 0..200_000usize {
            let x = if i % 2 == 0 {
                Complex::new(1.0, -1.0)
            } else {
                Complex::new(-1.0, 1.0)
            };
            let y = canceller.process(x);
            assert!(y.re.is_finite() && y.im.is_finite());
            max_norm = max_norm.max(y.norm());
        }
        assert!(max_norm < 10.0, "Unexpectedly large output norm: {}", max_norm);
    }
}
