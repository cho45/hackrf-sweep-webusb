use crate::filter::FirFilter;

#[derive(Clone, Copy, Debug, Default)]
pub struct FMStereoStats {
    pub pilot_level: f32,
    pub stereo_blend: f32,
    pub stereo_locked: bool,
    pub mono_fallback_count: u32,
    pub pll_phase_err_rad: f32,
    pub pll_freq_corr_hz: f32,
    pub pll_q_over_i: f32,
    pub pll_locked: bool,
}

const AUDIO_FIR_TAPS: usize = 128;
const AUDIO_LPF_CUTOFF_HZ: f32 = 15_000.0;
const PILOT_DETECT_LPF_HZ: f32 = 40.0;
const PLL_BW_HZ: f32 = 12.0;
const PLL_DAMPING: f32 = 0.707;
const PLL_MAX_FREQ_ERR_HZ: f32 = 400.0;
const PLL_LOCK_ERR_RAD: f32 = 0.35;
const PLL_LOCK_Q_OVER_I_MAX: f32 = 0.35;
const LR_PHASE_TRACK_LPF_HZ: f32 = 25.0;

/// FM MPX から L/R を復元する簡易ステレオデコーダ。
///
/// MPX の周波数配置（FM復調後）:
/// - 0..15kHz: `L+R`（モノラル成分）
/// - 19kHz: pilot
/// - 38kHz を中心とする `23..53kHz`（= 38±15kHz）: DSB-SC の `L-R`
///
/// 実装方針:
/// - pilot(19kHz) の位相を推定して 38kHz 同期検波の基準位相を作る
/// - `x * cos(2*pilot_phase)` で `L-R` をベースバンドへ落として LPF
/// - `L = (L+R) + (L-R)`, `R = (L+R) - (L-R)` で合成
///
/// - pilot(19kHz) は複素同期検波で位相を追従
/// - L-R は 38kHz 同期検波 + LPF
/// - pilot レベルに応じて stereo blend を自動調整し、ロック不十分時は mono に寄せる
pub struct FMStereoDecoder {
    sample_rate_hz: f32,
    pilot_phase: f32,
    pilot_omega: f32,
    pilot_lo_phase: f32,
    pilot_lo_omega: f32,
    pilot_hi_phase: f32,
    pilot_hi_omega: f32,

    pilot_i_lp: f32,
    pilot_q_lp: f32,
    pilot_i_lo_lp: f32,
    pilot_q_lo_lp: f32,
    pilot_i_hi_lp: f32,
    pilot_q_hi_lp: f32,
    pilot_lp_alpha: f32,
    pilot_phase_err_last: f32,
    pll_kp: f32,
    pll_ki: f32,
    pll_freq_corr: f32,
    pll_freq_corr_max: f32,

    dc_prev_x: f32,
    dc_prev_y: f32,
    dc_hp_a: f32,

    sum_lpf: FirFilter,
    diff_i_lpf: FirFilter,
    diff_q_lpf: FirFilter,
    lr2_re_lp: f32,
    lr2_im_lp: f32,
    lr_phase_track_alpha: f32,

    deemphasis_alpha: Option<f32>,
    deemphasis_l: f32,
    deemphasis_r: f32,

    pilot_level: f32,
    pilot_level_alpha: f32,
    pilot_mix_power: f32,
    pilot_power_alpha: f32,
    pilot_fraction: f32,
    pilot_fraction_alpha: f32,
    pilot_quality: f32,
    pilot_quality_alpha: f32,

    stereo_blend: f32,
    blend_attack_alpha: f32,
    blend_release_alpha: f32,

    pilot_lock_low: f32,
    pilot_lock_high: f32,
    pilot_fraction_low: f32,
    pilot_fraction_high: f32,
    pilot_quality_low: f32,
    pilot_quality_high: f32,
    stereo_lock_on: f32,
    stereo_lock_off: f32,

    stereo_locked: bool,
    mono_fallback_count: u32,
}

fn alpha_from_cutoff(sample_rate_hz: f32, cutoff_hz: f32) -> f32 {
    let dt = 1.0 / sample_rate_hz;
    let tau = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz.max(1.0));
    dt / (tau + dt)
}

fn alpha_from_tau(sample_rate_hz: f32, tau_sec: f32) -> f32 {
    let dt = 1.0 / sample_rate_hz;
    dt / (tau_sec.max(1e-9) + dt)
}

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

fn pilot_phase_error_from_iq(i: f32, q: f32) -> f32 {
    // 19k位相は 180度反転しても 38k 再生には等価なので、
    // 2倍角で誤差を求めて π 曖昧性を吸収する。
    let twice_err = (2.0 * i * q).atan2(i * i - q * q);
    0.5 * twice_err
}

impl FMStereoDecoder {
    pub fn new(sample_rate_hz: f32, deemphasis_tau_us: Option<f32>) -> Self {
        assert!(sample_rate_hz > 0.0, "sample_rate_hz must be > 0");

        let pilot_omega = 2.0 * std::f32::consts::PI * 19_000.0 / sample_rate_hz;
        let pilot_lo_omega = 2.0 * std::f32::consts::PI * 18_000.0 / sample_rate_hz;
        let pilot_hi_omega = 2.0 * std::f32::consts::PI * 20_000.0 / sample_rate_hz;
        let pilot_lp_alpha = alpha_from_cutoff(sample_rate_hz, PILOT_DETECT_LPF_HZ);
        let dc_hp_a = (-2.0 * std::f32::consts::PI * 30.0 / sample_rate_hz).exp();
        let pilot_level_alpha = alpha_from_tau(sample_rate_hz, 0.02);
        let pilot_power_alpha = alpha_from_tau(sample_rate_hz, 0.02);
        let pilot_fraction_alpha = alpha_from_tau(sample_rate_hz, 0.03);
        let pilot_quality_alpha = alpha_from_tau(sample_rate_hz, 0.05);
        let blend_attack_alpha = alpha_from_tau(sample_rate_hz, 0.03);
        let blend_release_alpha = alpha_from_tau(sample_rate_hz, 0.20);
        let lr_phase_track_alpha = alpha_from_cutoff(sample_rate_hz, LR_PHASE_TRACK_LPF_HZ);
        let wn = 2.0 * std::f32::consts::PI * PLL_BW_HZ / sample_rate_hz;
        let pll_kp = 2.0 * PLL_DAMPING * wn;
        let pll_ki = wn * wn;
        let pll_freq_corr_max = 2.0 * std::f32::consts::PI * PLL_MAX_FREQ_ERR_HZ / sample_rate_hz;
        let deemphasis_alpha = deemphasis_tau_us.and_then(|tau_us| {
            if tau_us <= 0.0 {
                return None;
            }
            Some(alpha_from_tau(sample_rate_hz, tau_us * 1e-6))
        });

        Self {
            sample_rate_hz,
            pilot_phase: 0.0,
            pilot_omega,
            pilot_lo_phase: 0.0,
            pilot_lo_omega,
            pilot_hi_phase: 0.0,
            pilot_hi_omega,
            pilot_i_lp: 0.0,
            pilot_q_lp: 0.0,
            pilot_i_lo_lp: 0.0,
            pilot_q_lo_lp: 0.0,
            pilot_i_hi_lp: 0.0,
            pilot_q_hi_lp: 0.0,
            pilot_lp_alpha,
            pilot_phase_err_last: 0.0,
            pll_kp,
            pll_ki,
            pll_freq_corr: 0.0,
            pll_freq_corr_max,
            dc_prev_x: 0.0,
            dc_prev_y: 0.0,
            dc_hp_a,
            sum_lpf: FirFilter::new_lowpass_hamming(
                AUDIO_FIR_TAPS,
                AUDIO_LPF_CUTOFF_HZ / sample_rate_hz,
            ),
            diff_i_lpf: FirFilter::new_lowpass_hamming(
                AUDIO_FIR_TAPS,
                AUDIO_LPF_CUTOFF_HZ / sample_rate_hz,
            ),
            diff_q_lpf: FirFilter::new_lowpass_hamming(
                AUDIO_FIR_TAPS,
                AUDIO_LPF_CUTOFF_HZ / sample_rate_hz,
            ),
            lr2_re_lp: 0.0,
            lr2_im_lp: 0.0,
            lr_phase_track_alpha,
            deemphasis_alpha,
            deemphasis_l: 0.0,
            deemphasis_r: 0.0,
            pilot_level: 0.0,
            pilot_level_alpha,
            pilot_mix_power: 0.0,
            pilot_power_alpha,
            pilot_fraction: 0.0,
            pilot_fraction_alpha,
            pilot_quality: 0.0,
            pilot_quality_alpha,
            stereo_blend: 0.0,
            blend_attack_alpha,
            blend_release_alpha,
            pilot_lock_low: 0.010,
            pilot_lock_high: 0.030,
            pilot_fraction_low: 0.006,
            pilot_fraction_high: 0.020,
            pilot_quality_low: 1.8,
            pilot_quality_high: 4.0,
            stereo_lock_on: 0.55,
            stereo_lock_off: 0.35,
            stereo_locked: false,
            mono_fallback_count: 0,
        }
    }

    pub fn reset(&mut self) {
        self.pilot_phase = 0.0;
        self.pilot_lo_phase = 0.0;
        self.pilot_hi_phase = 0.0;
        self.pilot_i_lp = 0.0;
        self.pilot_q_lp = 0.0;
        self.pilot_i_lo_lp = 0.0;
        self.pilot_q_lo_lp = 0.0;
        self.pilot_i_hi_lp = 0.0;
        self.pilot_q_hi_lp = 0.0;
        self.pilot_phase_err_last = 0.0;
        self.pll_freq_corr = 0.0;
        self.dc_prev_x = 0.0;
        self.dc_prev_y = 0.0;
        self.sum_lpf.reset();
        self.diff_i_lpf.reset();
        self.diff_q_lpf.reset();
        self.lr2_re_lp = 0.0;
        self.lr2_im_lp = 0.0;
        self.deemphasis_l = 0.0;
        self.deemphasis_r = 0.0;
        self.pilot_level = 0.0;
        self.pilot_mix_power = 0.0;
        self.pilot_fraction = 0.0;
        self.pilot_quality = 0.0;
        self.stereo_blend = 0.0;
        self.stereo_locked = false;
        self.mono_fallback_count = 0;
    }

    pub fn process(&mut self, mpx: &[f32], left: &mut Vec<f32>, right: &mut Vec<f32>) {
        left.clear();
        right.clear();
        if mpx.is_empty() {
            return;
        }

        left.reserve(mpx.len());
        right.reserve(mpx.len());

        for &raw in mpx {
            // MPXの低域DCは分離に悪影響なので軽く除去する。
            let x = raw - self.dc_prev_x + self.dc_hp_a * self.dc_prev_y;
            self.dc_prev_x = raw;
            self.dc_prev_y = x;

            let c19 = self.pilot_phase.cos();
            let s19 = self.pilot_phase.sin();

            // 19k pilot の複素包絡を推定し、位相オフセットを取り出す。
            // ここで得た位相を 2倍して 38k の同期検波器を作る。
            let pilot_i = x * c19;
            let pilot_q = -x * s19;
            let pilot_mix_power_inst = pilot_i * pilot_i + pilot_q * pilot_q;
            self.pilot_mix_power += self.pilot_power_alpha * (pilot_mix_power_inst - self.pilot_mix_power);
            self.pilot_i_lp += self.pilot_lp_alpha * (pilot_i - self.pilot_i_lp);
            self.pilot_q_lp += self.pilot_lp_alpha * (pilot_q - self.pilot_q_lp);

            // 近傍の 18kHz / 20kHz でも同様の同期検波を行い、ノイズ床推定に使う。
            let c18 = self.pilot_lo_phase.cos();
            let s18 = self.pilot_lo_phase.sin();
            let c20 = self.pilot_hi_phase.cos();
            let s20 = self.pilot_hi_phase.sin();
            let pilot_i_lo = x * c18;
            let pilot_q_lo = -x * s18;
            let pilot_i_hi = x * c20;
            let pilot_q_hi = -x * s20;
            self.pilot_i_lo_lp += self.pilot_lp_alpha * (pilot_i_lo - self.pilot_i_lo_lp);
            self.pilot_q_lo_lp += self.pilot_lp_alpha * (pilot_q_lo - self.pilot_q_lo_lp);
            self.pilot_i_hi_lp += self.pilot_lp_alpha * (pilot_i_hi - self.pilot_i_hi_lp);
            self.pilot_q_hi_lp += self.pilot_lp_alpha * (pilot_q_hi - self.pilot_q_hi_lp);

            let pilot_phase_err = pilot_phase_error_from_iq(self.pilot_i_lp, self.pilot_q_lp);
            self.pilot_phase_err_last = pilot_phase_err;
            // 2次PLL: 位相誤差でNCOの周波数補正を更新し、狭帯域で安定追従させる。
            self.pll_freq_corr =
                (self.pll_freq_corr + self.pll_ki * pilot_phase_err).clamp(-self.pll_freq_corr_max, self.pll_freq_corr_max);
            let pilot_phase_locked = self.pilot_phase + pilot_phase_err;
            let pilot_coherent_power = self.pilot_i_lp * self.pilot_i_lp + self.pilot_q_lp * self.pilot_q_lp;
            let pilot_side_power = 0.5
                * ((self.pilot_i_lo_lp * self.pilot_i_lo_lp + self.pilot_q_lo_lp * self.pilot_q_lo_lp)
                    + (self.pilot_i_hi_lp * self.pilot_i_hi_lp + self.pilot_q_hi_lp * self.pilot_q_hi_lp));
            let pilot_level_inst = pilot_coherent_power.sqrt() * 2.0;
            self.pilot_level += self.pilot_level_alpha * (pilot_level_inst - self.pilot_level);
            let pilot_fraction_inst = pilot_coherent_power / (self.pilot_mix_power + 1e-9);
            self.pilot_fraction += self.pilot_fraction_alpha * (pilot_fraction_inst - self.pilot_fraction);
            let pilot_quality_inst = pilot_coherent_power / (pilot_side_power + 1e-9);
            self.pilot_quality += self.pilot_quality_alpha * (pilot_quality_inst - self.pilot_quality);

            let level_denom = (self.pilot_lock_high - self.pilot_lock_low).max(1e-6);
            let frac_denom = (self.pilot_fraction_high - self.pilot_fraction_low).max(1e-6);
            let level_gate = clamp01((self.pilot_level - self.pilot_lock_low) / level_denom);
            let frac_gate = clamp01((self.pilot_fraction - self.pilot_fraction_low) / frac_denom);
            let quality_denom = (self.pilot_quality_high - self.pilot_quality_low).max(1e-6);
            let quality_gate = clamp01((self.pilot_quality - self.pilot_quality_low) / quality_denom);
            let target_blend = level_gate * quality_gate * frac_gate;
            let blend_alpha = if target_blend > self.stereo_blend {
                self.blend_attack_alpha
            } else {
                self.blend_release_alpha
            };
            self.stereo_blend += blend_alpha * (target_blend - self.stereo_blend);

            let locked_now = if self.stereo_locked {
                self.stereo_blend >= self.stereo_lock_off
            } else {
                self.stereo_blend >= self.stereo_lock_on
            };
            if self.stereo_locked && !locked_now {
                self.mono_fallback_count = self.mono_fallback_count.saturating_add(1);
            }
            self.stereo_locked = locked_now;

            // `L-R` は 38kHz 抑圧搬送波 (DSB-SC) なので、38k同期検波でベースバンドへ戻す。
            // 周波数領域では:
            // - 入力: 38±15kHz の帯域（23..53kHz）
            // - 同期検波後: 0..15kHz (+ 2*38k 近傍の高域項)
            // - 後段LPFで 0..15kHz を取り出す
            let c38 = (2.0 * pilot_phase_locked).cos();
            let s38 = (2.0 * pilot_phase_locked).sin();
            let lr_i_raw = 2.0 * x * c38;
            let lr_q_raw = -2.0 * x * s38;

            // 128tap Hamming FIR を明示的に通して `L+R`, `L-R` を抽出する。
            // - L+R: MPX低域(0..15kHz)
            // - L-R: 同期検波後の低域(0..15kHz)、I/QをそれぞれLPF
            let sum = self.sum_lpf.process_sample(x);
            let diff_i = self.diff_i_lpf.process_sample(lr_i_raw);
            let diff_q = self.diff_q_lpf.process_sample(lr_q_raw);

            // L-R複素成分の二乗平均から位相ズレを推定し、実軸へ寄せる。
            // z = s * exp(jδ) (s: 実信号) のとき、arg(E[z^2]) = 2δ を使う。
            let lr2_re = diff_i * diff_i - diff_q * diff_q;
            let lr2_im = 2.0 * diff_i * diff_q;
            self.lr2_re_lp += self.lr_phase_track_alpha * (lr2_re - self.lr2_re_lp);
            self.lr2_im_lp += self.lr_phase_track_alpha * (lr2_im - self.lr2_im_lp);
            let lr_phase_corr = 0.5 * self.lr2_im_lp.atan2(self.lr2_re_lp);
            let c_lr = lr_phase_corr.cos();
            let s_lr = lr_phase_corr.sin();
            let lr_aligned = diff_i * c_lr + diff_q * s_lr;

            // lock 品質に応じて blend を掛け、誤ロック時は差分成分を自動で弱める。
            let lr = lr_aligned * self.stereo_blend;
            let mut l = sum + lr;
            let mut r = sum - lr;

            if let Some(alpha) = self.deemphasis_alpha {
                self.deemphasis_l += alpha * (l - self.deemphasis_l);
                self.deemphasis_r += alpha * (r - self.deemphasis_r);
                l = self.deemphasis_l;
                r = self.deemphasis_r;
            }

            left.push(l);
            right.push(r);

            let loop_term = self.pll_freq_corr + self.pll_kp * pilot_phase_err;
            let phase_step = self.pilot_omega + loop_term;
            self.pilot_phase += phase_step;
            if self.pilot_phase >= 2.0 * std::f32::consts::PI {
                self.pilot_phase -= 2.0 * std::f32::consts::PI;
            } else if self.pilot_phase < 0.0 {
                self.pilot_phase += 2.0 * std::f32::consts::PI;
            }
            self.pilot_lo_phase += self.pilot_lo_omega + loop_term;
            if self.pilot_lo_phase >= 2.0 * std::f32::consts::PI {
                self.pilot_lo_phase -= 2.0 * std::f32::consts::PI;
            } else if self.pilot_lo_phase < 0.0 {
                self.pilot_lo_phase += 2.0 * std::f32::consts::PI;
            }
            self.pilot_hi_phase += self.pilot_hi_omega + loop_term;
            if self.pilot_hi_phase >= 2.0 * std::f32::consts::PI {
                self.pilot_hi_phase -= 2.0 * std::f32::consts::PI;
            } else if self.pilot_hi_phase < 0.0 {
                self.pilot_hi_phase += 2.0 * std::f32::consts::PI;
            }
        }
    }

    pub fn stats(&self) -> FMStereoStats {
        let i_abs = self.pilot_i_lp.abs();
        let q_abs = self.pilot_q_lp.abs();
        let q_over_i = q_abs / (i_abs + 1e-9);
        let phase_err_abs = self.pilot_phase_err_last.abs();
        let pll_locked = self.pilot_level >= self.pilot_lock_low
            && phase_err_abs < PLL_LOCK_ERR_RAD
            && q_over_i < PLL_LOCK_Q_OVER_I_MAX;
        let pll_freq_corr_hz =
            self.pll_freq_corr * self.sample_rate_hz / (2.0 * std::f32::consts::PI);
        FMStereoStats {
            pilot_level: self.pilot_level,
            stereo_blend: self.stereo_blend,
            stereo_locked: self.stereo_locked,
            mono_fallback_count: self.mono_fallback_count,
            pll_phase_err_rad: self.pilot_phase_err_last,
            pll_freq_corr_hz,
            pll_q_over_i: q_over_i,
            pll_locked,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_stereo_mpx(fs: f32, len: usize) -> Vec<f32> {
        let mut mpx = Vec::with_capacity(len);
        for i in 0..len {
            let t = i as f32 / fs;
            let l = 0.5 * (2.0 * std::f32::consts::PI * 1_000.0 * t).sin();
            let r = 0.5 * (2.0 * std::f32::consts::PI * 2_000.0 * t).sin();
            let lp = l + r;
            let lr = l - r;
            let pilot = 0.10 * (2.0 * std::f32::consts::PI * 19_000.0 * t).cos();
            let dsb = lr * (2.0 * std::f32::consts::PI * 38_000.0 * t).cos();
            mpx.push(0.45 * lp + pilot + 0.45 * dsb);
        }
        mpx
    }

    fn build_stereo_mpx_from_program(fs: f32, left: &[f32], right: &[f32]) -> Vec<f32> {
        assert_eq!(left.len(), right.len());
        let mut mpx = Vec::with_capacity(left.len());
        for i in 0..left.len() {
            let t = i as f32 / fs;
            let lp = left[i] + right[i];
            let lr = left[i] - right[i];
            let pilot = 0.10 * (2.0 * std::f32::consts::PI * 19_000.0 * t).cos();
            let dsb = lr * (2.0 * std::f32::consts::PI * 38_000.0 * t).cos();
            mpx.push(0.45 * lp + pilot + 0.45 * dsb);
        }
        mpx
    }

    fn build_stereo_mpx_from_program_with_phase(
        fs: f32,
        left: &[f32],
        right: &[f32],
        pilot_phase: f32,
    ) -> Vec<f32> {
        assert_eq!(left.len(), right.len());
        let mut mpx = Vec::with_capacity(left.len());
        for i in 0..left.len() {
            let t = i as f32 / fs;
            let lp = left[i] + right[i];
            let lr = left[i] - right[i];
            let pilot = 0.10 * (2.0 * std::f32::consts::PI * 19_000.0 * t + pilot_phase).cos();
            let dsb =
                lr * (2.0 * std::f32::consts::PI * 38_000.0 * t + 2.0 * pilot_phase).cos();
            mpx.push(0.45 * lp + pilot + 0.45 * dsb);
        }
        mpx
    }

    fn build_program_signal(fs: f32, len: usize, freqs_hz: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0f32; len];
        for (k, &f) in freqs_hz.iter().enumerate() {
            let amp = 0.20 / (1.0 + k as f32 * 0.35);
            for (n, y) in out.iter_mut().enumerate() {
                let t = n as f32 / fs;
                *y += amp * (2.0 * std::f32::consts::PI * f * t).sin();
            }
        }
        out
    }

    fn build_noise(len: usize, seed: u32, gain: f32) -> Vec<f32> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                let u = ((state >> 8) as f32) * (1.0 / 16_777_216.0); // [0,1)
                (u * 2.0 - 1.0) * gain
            })
            .collect()
    }

    fn build_mono_with_pilot_mpx(fs: f32, len: usize, noise_gain: f32) -> Vec<f32> {
        let noise = build_noise(len, 0x8765_4321, noise_gain);
        let mut mpx = Vec::with_capacity(len);
        for i in 0..len {
            let t = i as f32 / fs;
            let mono = 0.55 * (2.0 * std::f32::consts::PI * 1_000.0 * t).sin();
            let pilot = 0.10 * (2.0 * std::f32::consts::PI * 19_000.0 * t).cos();
            mpx.push(mono + pilot + noise[i]);
        }
        mpx
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        (samples.iter().map(|v| v * v).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn estimate_mix_coeffs(y: &[f32], x1: &[f32], x2: &[f32]) -> (f32, f32) {
        assert_eq!(y.len(), x1.len());
        assert_eq!(y.len(), x2.len());

        let mut s11 = 0.0f32;
        let mut s22 = 0.0f32;
        let mut s12 = 0.0f32;
        let mut t1 = 0.0f32;
        let mut t2 = 0.0f32;

        for i in 0..y.len() {
            let a = x1[i];
            let b = x2[i];
            let v = y[i];
            s11 += a * a;
            s22 += b * b;
            s12 += a * b;
            t1 += a * v;
            t2 += b * v;
        }
        let det = s11 * s22 - s12 * s12;
        if det.abs() < 1e-9 {
            return (0.0, 0.0);
        }
        let inv = 1.0 / det;
        let c1 = inv * (s22 * t1 - s12 * t2);
        let c2 = inv * (-s12 * t1 + s11 * t2);
        (c1, c2)
    }

    fn ratio_db(main: f32, leak: f32) -> f32 {
        let num = main.abs().max(1e-9);
        let den = leak.abs().max(1e-9);
        20.0 * (num / den).log10()
    }

    #[test]
    fn pilot_phase_detector_polarity_is_input_minus_local() {
        // pilot_i=cos(θ-φ), pilot_q=+sin(θ-φ) なので、
        // 検出誤差は (θ-φ) になるべき。
        for delta in [-1.2f32, -0.7, -0.2, 0.2, 0.7, 1.2] {
            let i = delta.cos();
            let q = delta.sin();
            let err = pilot_phase_error_from_iq(i, q);
            let expected = delta;
            let abs_err = (err - expected).abs();
            assert!(
                abs_err < 1e-4,
                "phase detector polarity mismatch: delta={} err={} expected={} abs_err={}",
                delta,
                err,
                expected,
                abs_err
            );
        }
    }

    #[test]
    fn pll_tracks_pilot_frequency_offset() {
        let fs = 200_000.0f32;
        let n = 220_000usize;
        for pilot_hz in [19_030.0f32, 18_970.0f32] {
            let mut mpx = Vec::with_capacity(n);
            for i in 0..n {
                let t = i as f32 / fs;
                let pilot = 0.18 * (2.0 * std::f32::consts::PI * pilot_hz * t).cos();
                // 現実に近づけるため、低域プログラム成分を少量足す。
                let mono = 0.10 * (2.0 * std::f32::consts::PI * 1_200.0 * t).sin();
                mpx.push(mono + pilot);
            }

            let mut dec = FMStereoDecoder::new(fs, None);
            let mut l = Vec::new();
            let mut r = Vec::new();
            for chunk in mpx.chunks(4096) {
                dec.process(chunk, &mut l, &mut r);
            }

            let expected = 2.0 * std::f32::consts::PI * (pilot_hz - 19_000.0) / fs;
            let corr = dec.pll_freq_corr;
            let corr_abs_err = (corr.abs() - expected.abs()).abs();
            assert!(
                corr.abs() > 1e-4,
                "PLL frequency correction did not move: got={} expected={} pilot_hz={}",
                corr,
                expected,
                pilot_hz
            );
            assert!(
                corr.signum() == expected.signum(),
                "PLL correction sign mismatch: got={} expected={} pilot_hz={}",
                corr,
                expected,
                pilot_hz
            );
            assert!(
                corr_abs_err < 0.0012,
                "PLL frequency correction mismatch: got={} expected={} abs_err={} pilot_hz={}",
                corr,
                expected,
                corr_abs_err,
                pilot_hz
            );
        }
    }

    #[test]
    fn pll_lock_aligns_pilot_to_i_axis() {
        let fs = 200_000.0f32;
        let n = 220_000usize;
        let pilot_phase = 0.9f32;
        let mut mpx = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f32 / fs;
            let pilot = 0.18 * (2.0 * std::f32::consts::PI * 19_000.0 * t + pilot_phase).cos();
            let mono = 0.05 * (2.0 * std::f32::consts::PI * 1_000.0 * t).sin();
            mpx.push(mono + pilot);
        }

        let mut dec = FMStereoDecoder::new(fs, None);
        let mut l = Vec::new();
        let mut r = Vec::new();
        for chunk in mpx.chunks(4096) {
            dec.process(chunk, &mut l, &mut r);
        }

        let i_abs = dec.pilot_i_lp.abs();
        let q_abs = dec.pilot_q_lp.abs();
        let q_over_i = q_abs / (i_abs + 1e-9);
        assert!(
            q_over_i < 0.35,
            "PLL did not align pilot on I-axis: i_lp={} q_lp={} q_over_i={}",
            dec.pilot_i_lp,
            dec.pilot_q_lp,
            q_over_i
        );
        assert!(
            dec.pilot_phase_err_last.abs() < 0.35,
            "PLL phase error did not converge near zero: err={}",
            dec.pilot_phase_err_last
        );
    }

    #[test]
    fn stereo_separation_is_stable_with_pilot_phase_offset() {
        let fs = 200_000.0f32;
        let n = 240_000usize;
        let pilot_phase = 1.1f32;
        let left_src = build_program_signal(fs, n, &[700.0, 1_300.0, 2_100.0, 3_700.0]);
        let right_src = build_program_signal(fs, n, &[900.0, 1_700.0, 2_900.0, 4_300.0]);
        let mpx = build_stereo_mpx_from_program_with_phase(fs, &left_src, &right_src, pilot_phase);

        let mut dec = FMStereoDecoder::new(fs, None);
        let mut l_out = Vec::new();
        let mut r_out = Vec::new();
        dec.process(&mpx, &mut l_out, &mut r_out);

        let skip = 30_000usize;
        let l = &l_out[skip..];
        let r = &r_out[skip..];
        let left_ref = &left_src[skip..];
        let right_ref = &right_src[skip..];

        let (ll, lr) = estimate_mix_coeffs(l, left_ref, right_ref);
        let (rl, rr) = estimate_mix_coeffs(r, left_ref, right_ref);
        let sep_l_db = ratio_db(ll, lr);
        let sep_r_db = ratio_db(rr, rl);

        assert!(
            sep_l_db > 6.0,
            "left separation too low with pilot phase offset: ll={} lr={} sep_l_db={}",
            ll,
            lr,
            sep_l_db
        );
        assert!(
            sep_r_db > 6.0,
            "right separation too low with pilot phase offset: rr={} rl={} sep_r_db={}",
            rr,
            rl,
            sep_r_db
        );
    }

    #[test]
    fn stereo_separation_matrix_has_reasonable_crosstalk_db() {
        let fs = 200_000.0f32;
        let n = 240_000usize;
        let left_src = build_program_signal(fs, n, &[700.0, 1_300.0, 2_100.0, 3_700.0]);
        let right_src = build_program_signal(fs, n, &[900.0, 1_700.0, 2_900.0, 4_300.0]);
        let mpx = build_stereo_mpx_from_program(fs, &left_src, &right_src);

        let mut dec = FMStereoDecoder::new(fs, None);
        let mut l_out = Vec::new();
        let mut r_out = Vec::new();
        dec.process(&mpx, &mut l_out, &mut r_out);

        let skip = 30_000usize;
        let l = &l_out[skip..];
        let r = &r_out[skip..];
        let left_ref = &left_src[skip..];
        let right_ref = &right_src[skip..];

        let (ll, lr) = estimate_mix_coeffs(l, left_ref, right_ref);
        let (rl, rr) = estimate_mix_coeffs(r, left_ref, right_ref);
        let sep_l_db = ratio_db(ll, lr);
        let sep_r_db = ratio_db(rr, rl);

        assert!(
            sep_l_db > 6.0,
            "left channel separation too low: ll={} lr={} sep_l_db={}",
            ll,
            lr,
            sep_l_db
        );
        assert!(
            sep_r_db > 6.0,
            "right channel separation too low: rr={} rl={} sep_r_db={}",
            rr,
            rl,
            sep_r_db
        );

        let st = dec.stats();
        assert!(st.stereo_blend > 0.5, "stereo blend did not rise enough: {}", st.stereo_blend);
        assert!(st.stereo_locked, "stereo did not lock");
    }

    #[test]
    fn clean_stereo_reaches_high_blend() {
        let fs = 200_000.0f32;
        let mpx = build_stereo_mpx(fs, 220_000);
        let mut dec = FMStereoDecoder::new(fs, None);
        let mut l = Vec::new();
        let mut r = Vec::new();
        dec.process(&mpx, &mut l, &mut r);

        let st = dec.stats();
        assert!(st.stereo_locked, "stereo should lock on clean multiplex: {:?}", st);
        assert!(
            st.stereo_blend > 0.75,
            "stereo blend too low on clean multiplex: {:?}",
            st
        );
    }

    #[test]
    fn mono_program_with_pilot_keeps_lr_difference_low() {
        let fs = 200_000.0f32;
        let mpx = build_mono_with_pilot_mpx(fs, 220_000, 0.01);

        let mut dec = FMStereoDecoder::new(fs, None);
        let mut l_out = Vec::new();
        let mut r_out = Vec::new();
        dec.process(&mpx, &mut l_out, &mut r_out);

        let skip = 20_000usize;
        let l = &l_out[skip..];
        let r = &r_out[skip..];
        let mut sum = Vec::with_capacity(l.len());
        let mut diff = Vec::with_capacity(l.len());
        for i in 0..l.len() {
            sum.push(0.5 * (l[i] + r[i]));
            diff.push(0.5 * (l[i] - r[i]));
        }

        let sum_rms = rms(&sum);
        let diff_rms = rms(&diff);
        assert!(sum_rms > 1e-4, "sum rms too small");
        assert!(
            diff_rms < sum_rms * 0.12,
            "mono+pilot should keep L-R low, got sum_rms={} diff_rms={}",
            sum_rms,
            diff_rms
        );
    }

    #[test]
    fn mono_program_with_pilot_under_higher_noise_limits_lr_leakage() {
        let fs = 200_000.0f32;
        let mpx = build_mono_with_pilot_mpx(fs, 220_000, 0.05);

        let mut dec = FMStereoDecoder::new(fs, None);
        let mut l_out = Vec::new();
        let mut r_out = Vec::new();
        dec.process(&mpx, &mut l_out, &mut r_out);

        let skip = 20_000usize;
        let l = &l_out[skip..];
        let r = &r_out[skip..];
        let mut sum = Vec::with_capacity(l.len());
        let mut diff = Vec::with_capacity(l.len());
        for i in 0..l.len() {
            sum.push(0.5 * (l[i] + r[i]));
            diff.push(0.5 * (l[i] - r[i]));
        }
        let sum_rms = rms(&sum);
        let diff_rms = rms(&diff);

        assert!(sum_rms > 1e-4, "sum rms too small");
        assert!(
            diff_rms < sum_rms * 0.20,
            "mono+pilot (higher noise) leaks too much L-R: sum_rms={} diff_rms={}",
            sum_rms,
            diff_rms
        );
    }

    #[test]
    fn fallback_counter_increments_when_lock_is_lost() {
        let fs = 200_000.0f32;
        let mut dec = FMStereoDecoder::new(fs, None);
        let n = 120_000usize;

        let with_pilot = build_stereo_mpx(fs, n);

        let no_pilot: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / fs;
                0.3 * (2.0 * std::f32::consts::PI * 1_000.0 * t).sin()
            })
            .collect();

        let mut l = Vec::new();
        let mut r = Vec::new();
        dec.process(&with_pilot, &mut l, &mut r);
        let st1 = dec.stats();
        assert!(
            st1.stereo_blend > 0.5,
            "blend should rise with stereo multiplex input, got {}",
            st1.stereo_blend
        );

        dec.process(&no_pilot, &mut l, &mut r);
        let st2 = dec.stats();
        assert!(st2.mono_fallback_count >= 1, "fallback count did not increment");
        assert!(st2.stereo_blend < st1.stereo_blend, "blend should decay after pilot loss");
    }

    #[test]
    fn does_not_lock_without_pilot_on_single_tone() {
        let fs = 200_000.0f32;
        let mut dec = FMStereoDecoder::new(fs, None);
        let n = 240_000usize;

        let mono_tone: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / fs;
                0.6 * (2.0 * std::f32::consts::PI * 1_000.0 * t).sin()
            })
            .collect();

        let mut l = Vec::new();
        let mut r = Vec::new();
        dec.process(&mono_tone, &mut l, &mut r);

        let st = dec.stats();
        assert!(
            !st.stereo_locked,
            "decoder locked without pilot on single-tone input: {:?}",
            st
        );
        assert!(
            st.stereo_blend < 0.1,
            "stereo blend should stay near mono without pilot: {:?}",
            st
        );
    }

    #[test]
    fn does_not_lock_without_pilot_on_wideband_noise() {
        let fs = 200_000.0f32;
        let mut dec = FMStereoDecoder::new(fs, None);
        let n = 240_000usize;

        let noise = build_noise(n, 0x1234_5678, 0.7);

        let mut l = Vec::new();
        let mut r = Vec::new();
        dec.process(&noise, &mut l, &mut r);

        let st = dec.stats();
        assert!(
            !st.stereo_locked,
            "decoder locked without pilot on noise input: {:?}",
            st
        );
        assert!(
            st.stereo_blend < 0.1,
            "stereo blend should not pass lock threshold without pilot: {:?}",
            st
        );
    }

    #[test]
    fn does_not_lock_on_chunked_noise_across_seeds() {
        let fs = 200_000.0f32;
        for seed in 1..=8u32 {
            let mut dec = FMStereoDecoder::new(fs, None);
            let noise = build_noise(200_000, 0x1000_0000u32.wrapping_add(seed), 0.9);
            let mut l = Vec::new();
            let mut r = Vec::new();
            let mut peak_blend = 0.0f32;

            for chunk in noise.chunks(4096) {
                dec.process(chunk, &mut l, &mut r);
                peak_blend = peak_blend.max(dec.stats().stereo_blend);
            }

            let st = dec.stats();
            assert!(
                !st.stereo_locked,
                "decoder locked on chunked noise (seed={}): {:?}",
                seed,
                st
            );
            assert!(
                peak_blend < 0.35,
                "stereo blend spiked too high on chunked noise (seed={}): peak={}",
                seed,
                peak_blend
            );
        }
    }

    #[test]
    fn stays_locked_on_pure_19khz_pilot_after_lock() {
        let fs = 200_000.0f32;
        let mut dec = FMStereoDecoder::new(fs, None);
        let lock_input = build_stereo_mpx(fs, 180_000);
        let n = 240_000usize;

        let pure_pilot_like: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / fs;
                0.5 * (2.0 * std::f32::consts::PI * 19_000.0 * t).cos()
            })
            .collect();

        let mut l = Vec::new();
        let mut r = Vec::new();
        dec.process(&lock_input, &mut l, &mut r);
        let st_lock = dec.stats();
        assert!(st_lock.stereo_locked, "decoder did not lock before pilot-only segment: {:?}", st_lock);

        dec.process(&pure_pilot_like, &mut l, &mut r);
        let st = dec.stats();

        assert!(
            st.stereo_locked,
            "decoder unlocked on pilot-only segment: {:?}",
            st
        );
        assert!(
            st.stereo_blend > 0.5,
            "stereo blend dropped too much on pilot-only segment: {:?}",
            st
        );
    }

    #[test]
    fn unlocks_after_retune_to_no_signal() {
        let fs = 200_000.0f32;
        let mut dec = FMStereoDecoder::new(fs, None);

        let lock_input = build_stereo_mpx(fs, 180_000);
        let no_signal = vec![0.0f32; 260_000];

        let mut l = Vec::new();
        let mut r = Vec::new();
        dec.process(&lock_input, &mut l, &mut r);
        let st_lock = dec.stats();
        assert!(
            st_lock.stereo_locked,
            "decoder did not lock before retune: {:?}",
            st_lock
        );
        assert!(
            st_lock.stereo_blend > 0.8,
            "decoder did not reach strong lock blend before retune: {:?}",
            st_lock
        );

        dec.process(&no_signal, &mut l, &mut r);
        let st_after = dec.stats();
        assert!(
            !st_after.stereo_locked,
            "decoder stayed locked after retune to no signal: {:?}",
            st_after
        );
        assert!(
            st_after.stereo_blend < 0.1,
            "stereo blend did not decay enough after retune to no signal: {:?}",
            st_after
        );
        assert!(
            st_after.mono_fallback_count >= st_lock.mono_fallback_count + 1,
            "fallback count did not increment on unlock: before={:?} after={:?}",
            st_lock,
            st_after
        );
    }

    #[test]
    fn unlocks_after_retune_to_no_signal_in_chunks() {
        let fs = 200_000.0f32;
        let mut dec = FMStereoDecoder::new(fs, None);

        let lock_input = build_stereo_mpx(fs, 180_000);
        let no_signal = vec![0.0f32; 260_000];
        let mut l = Vec::new();
        let mut r = Vec::new();

        for chunk in lock_input.chunks(4096) {
            dec.process(chunk, &mut l, &mut r);
        }
        let st_lock = dec.stats();
        assert!(
            st_lock.stereo_locked,
            "decoder did not lock in chunked mode before retune: {:?}",
            st_lock
        );

        for chunk in no_signal.chunks(4096) {
            dec.process(chunk, &mut l, &mut r);
        }
        let st_after = dec.stats();
        assert!(
            !st_after.stereo_locked,
            "decoder stayed locked after chunked retune to no signal: {:?}",
            st_after
        );
        assert!(
            st_after.stereo_blend < 0.1,
            "stereo blend did not decay enough in chunked retune: {:?}",
            st_after
        );
    }
}
