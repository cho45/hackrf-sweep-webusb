use num_complex::Complex;

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
