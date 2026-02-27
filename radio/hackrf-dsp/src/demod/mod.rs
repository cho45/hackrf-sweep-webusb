use num_complex::Complex;
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
use std::arch::wasm32::{f32x4, f32x4_add, f32x4_mul, f32x4_sub, i32x4_shuffle, v128};

pub mod am;
pub mod fm;
pub mod fm_stereo;

pub use am::AMDemodulator;
pub use fm::FMDemodulator;
pub use fm_stereo::{FMStereoDecoder, FMStereoStats};

/// 位相加算ベースのNCO（周波数補正を毎サンプルで加える用途向け）。
pub struct PhaseNco {
    phase: f32,
    omega: f32,
}

impl PhaseNco {
    #[inline(always)]
    pub fn new(freq_hz: f32, sample_rate: f32) -> Self {
        let omega = 2.0 * std::f32::consts::PI * freq_hz / sample_rate;
        Self { phase: 0.0, omega }
    }

    #[inline(always)]
    pub fn reset(&mut self) {
        self.phase = 0.0;
    }

    #[inline(always)]
    pub fn sin_cos(&self) -> (f32, f32) {
        self.phase.sin_cos()
    }

    #[inline(always)]
    pub fn advance(&mut self, extra_omega: f32) {
        self.phase += self.omega + extra_omega;
        if self.phase >= 2.0 * std::f32::consts::PI {
            self.phase -= 2.0 * std::f32::consts::PI;
        } else if self.phase < 0.0 {
            self.phase += 2.0 * std::f32::consts::PI;
        }
    }

    /// 現在位相の sin/cos を返し、次サンプルに向けて位相を進める。
    #[inline(always)]
    pub fn sin_cos_and_advance(&mut self, extra_omega: f32) -> (f32, f32) {
        let out = self.phase.sin_cos();
        self.advance(extra_omega);
        out
    }
}

/// Number Controlled Oscillator (NCO)
/// 複素ベースバンド変換のための内部発振器
pub struct Nco {
    osc: Complex<f32>,
    phase_inc: Complex<f32>,
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    phase_pairs: [v128; 4],
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    phase_inc8: Complex<f32>,
    renorm_counter: u32,
}

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline]
fn complex_mul_interleaved2_simd(input: v128, osc: v128) -> v128 {
    let osc_swapped = i32x4_shuffle::<1, 0, 3, 2>(osc, osc);

    let prod_re = f32x4_mul(input, osc);
    let prod_im = f32x4_mul(input, osc_swapped);

    let prod_re_swapped = i32x4_shuffle::<1, 0, 3, 2>(prod_re, prod_re);
    let prod_im_swapped = i32x4_shuffle::<1, 0, 3, 2>(prod_im, prod_im);

    let re = f32x4_sub(prod_re, prod_re_swapped);
    let im = f32x4_add(prod_im, prod_im_swapped);

    i32x4_shuffle::<0, 4, 2, 6>(re, im)
}

impl Nco {
    pub fn new(freq_hz: f32, sample_rate: f32) -> Self {
        let dphi = 2.0 * std::f32::consts::PI * freq_hz / sample_rate;
        let phase_inc = Complex::new(dphi.cos(), dphi.sin());
        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        let (phase_pairs, phase_inc8) = {
            let mut p = Complex::new(1.0, 0.0);
            let mut powers = [Complex::new(0.0, 0.0); 8];
            for slot in &mut powers {
                *slot = p;
                p *= phase_inc;
            }
            (
                [
                    f32x4(powers[0].re, powers[0].im, powers[1].re, powers[1].im),
                    f32x4(powers[2].re, powers[2].im, powers[3].re, powers[3].im),
                    f32x4(powers[4].re, powers[4].im, powers[5].re, powers[5].im),
                    f32x4(powers[6].re, powers[6].im, powers[7].re, powers[7].im),
                ],
                p,
            )
        };
        Self {
            osc: Complex::new(1.0, 0.0),
            phase_inc,
            #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
            phase_pairs,
            #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
            phase_inc8,
            renorm_counter: 0,
        }
    }

    /// 1サンプル進め、その時点での複素発振値 e^(j * phase) を返す。
    /// これを元の入力信号(Complex)と掛け合わせることで、周波数シフト（ベースバンド変換）を行う。
    pub fn step(&mut self) -> Complex<f32> {
        let val = self.osc;
        self.osc *= self.phase_inc;
        self.renorm_counter = self.renorm_counter.wrapping_add(1);

        // 浮動小数誤差で |osc| がずれるため、定期的に正規化する
        if self.renorm_counter >= 1024 {
            self.renorm_counter = 0;
            let norm = self.osc.norm();
            if norm > 1e-12 {
                self.osc /= norm;
            } else {
                self.osc = Complex::new(1.0, 0.0);
            }
        }
        val
    }

    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    #[inline]
    pub fn step8_interleaved(&mut self) -> (v128, v128, v128, v128) {
        let osc_pair = f32x4(self.osc.re, self.osc.im, self.osc.re, self.osc.im);
        let n0 = complex_mul_interleaved2_simd(osc_pair, self.phase_pairs[0]);
        let n1 = complex_mul_interleaved2_simd(osc_pair, self.phase_pairs[1]);
        let n2 = complex_mul_interleaved2_simd(osc_pair, self.phase_pairs[2]);
        let n3 = complex_mul_interleaved2_simd(osc_pair, self.phase_pairs[3]);

        self.osc *= self.phase_inc8;
        self.renorm_counter = self.renorm_counter.wrapping_add(8);
        if self.renorm_counter >= 1024 {
            self.renorm_counter = 0;
            let norm = self.osc.norm();
            if norm > 1e-12 {
                self.osc /= norm;
            } else {
                self.osc = Complex::new(1.0, 0.0);
            }
        }

        (n0, n1, n2, n3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_phase_nco_advance_with_extra() {
        let sample_rate = 1000.0f32;
        let mut nco = PhaseNco::new(250.0, sample_rate);
        let (_, c0) = nco.sin_cos();
        assert!((c0 - 1.0).abs() < 1e-6);

        nco.advance(0.0);
        let (s1, c1) = nco.sin_cos();
        assert!((s1 - 1.0).abs() < 1e-6);
        assert!(c1.abs() < 1e-6);

        // 追加位相でさらに45度進む
        nco.advance(std::f32::consts::PI / 4.0);
        let (s2, c2) = nco.sin_cos();
        assert!((s2 - c2).abs() < 1e-5); // 225度付近
        assert!(s2 < 0.0 && c2 < 0.0);
    }
}
