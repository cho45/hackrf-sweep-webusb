use num_complex::Complex;
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
use std::arch::wasm32::{f32x4, v128};

pub mod am;
pub mod fm;

pub use am::AMDemodulator;
pub use fm::FMDemodulator;

/// Number Controlled Oscillator (NCO)
/// 複素ベースバンド変換のための内部発振器
pub struct Nco {
    osc: Complex<f32>,
    phase_inc: Complex<f32>,
    renorm_counter: u32,
}

impl Nco {
    pub fn new(freq_hz: f32, sample_rate: f32) -> Self {
        let dphi = 2.0 * std::f32::consts::PI * freq_hz / sample_rate;
        Self {
            osc: Complex::new(1.0, 0.0),
            phase_inc: Complex::new(dphi.cos(), dphi.sin()),
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
        let mut osc = self.osc;
        let inc = self.phase_inc;

        let a0 = osc;
        osc *= inc;
        let a1 = osc;
        osc *= inc;
        let a2 = osc;
        osc *= inc;
        let a3 = osc;
        osc *= inc;
        let a4 = osc;
        osc *= inc;
        let a5 = osc;
        osc *= inc;
        let a6 = osc;
        osc *= inc;
        let a7 = osc;
        osc *= inc;

        self.osc = osc;
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

        (
            f32x4(a0.re, a0.im, a1.re, a1.im),
            f32x4(a2.re, a2.im, a3.re, a3.im),
            f32x4(a4.re, a4.im, a5.re, a5.im),
            f32x4(a6.re, a6.im, a7.re, a7.im),
        )
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
}
