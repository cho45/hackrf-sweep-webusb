use super::PhaseNco;
use crate::filter::{ComplexFirFilter, FirFilter};
use crate::resample::Resampler;
use num_complex::Complex;

pub const FM_STEREO_INTERMEDIATE_RATE_HZ: f32 = 125_000.0;
pub const FM_STEREO_MPX_RESAMPLE_CUTOFF_HZ: f32 = 60_000.0;

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

const SUM_AUDIO_FIR_TAPS: usize = 128;
const DIFF_AUDIO_FIR_TAPS: usize = 128;
const SUM_AUDIO_LPF_CUTOFF_HZ: f32 = 15_000.0;
const DIFF_AUDIO_LPF_CUTOFF_HZ: f32 = 13_500.0;
const PILOT_DETECT_LPF_HZ: f32 = 40.0;
const PLL_BW_HZ: f32 = 12.0;
const PLL_DAMPING: f32 = 0.707;
const PLL_MAX_FREQ_ERR_HZ: f32 = 400.0;
const PLL_LOCK_ERR_RAD: f32 = 0.35;
const PLL_LOCK_Q_OVER_I_MAX: f32 = 0.35;
const PLL_UPDATE_INTERVAL: usize = 2;
const LR_PHASE_TRACK_LPF_HZ: f32 = 25.0;
const LR_PHASE_TRACK_UPDATE_INTERVAL: usize = 8;
const PILOT_SIDE_UPDATE_INTERVAL: usize = 4;

/// FM MPX から L/R を復元するステレオデコーダ。
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
    cfg: FMStereoConfig,
    mpx_input: MpxInputState,
    nco: NcoState,
    pilot: PilotState,
    pll: PllState,
    dc: DcBlockState,
    filters: FilterState,
    lr: LrState,
    audio: AudioState,
    blend: BlendState,
}

struct MpxInputState {
    resampler: Option<Resampler>,
    buffer: Vec<f32>,
}

struct FMStereoConfig {
    sample_rate_hz: f32,
    pilot_lp_alpha: f32,
    pilot_lp_alpha_side: f32,
    pilot_level_alpha: f32,
    pilot_power_alpha: f32,
    pilot_fraction_alpha: f32,
    pilot_quality_alpha: f32,
    blend_attack_alpha: f32,
    blend_release_alpha: f32,
    lr_phase_track_alpha: f32,
    deemphasis_alpha: Option<f32>,
    pilot_lock_low: f32,
    pilot_lock_high: f32,
    pilot_fraction_low: f32,
    pilot_fraction_high: f32,
    pilot_quality_low: f32,
    pilot_quality_high: f32,
    stereo_lock_on: f32,
    stereo_lock_off: f32,
}

struct NcoState {
    pilot: PhaseNco,
    pilot_lo: PhaseNco,
    pilot_hi: PhaseNco,
}

#[derive(Default)]
struct PilotState {
    i_lp: f32,
    q_lp: f32,
    i_lo_lp: f32,
    q_lo_lp: f32,
    i_hi_lp: f32,
    q_hi_lp: f32,
    level: f32,
    mix_power: f32,
    fraction: f32,
    quality: f32,
    side_update_countdown: usize,
}

struct PllState {
    phase_err_last: f32,
    kp: f32,
    ki: f32,
    freq_corr: f32,
    freq_corr_max: f32,
    update_countdown: usize,
    mix_cos2err: f32,
    mix_sin2err: f32,
}

struct DcBlockState {
    prev_x: f32,
    prev_y: f32,
    hp_a: f32,
}

struct FilterState {
    sum_lpf: FirFilter,
    diff_lpf: ComplexFirFilter,
}

#[derive(Default)]
struct LrState {
    re_lp: f32,
    im_lp: f32,
    corr_cos: f32,
    corr_sin: f32,
    update_countdown: usize,
}

#[derive(Default)]
struct AudioState {
    deemphasis_l: f32,
    deemphasis_r: f32,
}

#[derive(Default)]
struct BlendState {
    stereo_blend: f32,
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

impl FMStereoConfig {
    fn new(sample_rate_hz: f32, deemphasis_tau_us: Option<f32>) -> Self {
        let pilot_lp_alpha = alpha_from_cutoff(sample_rate_hz, PILOT_DETECT_LPF_HZ);
        let deemphasis_alpha = deemphasis_tau_us.and_then(|tau_us| {
            if tau_us <= 0.0 {
                None
            } else {
                Some(alpha_from_tau(sample_rate_hz, tau_us * 1e-6))
            }
        });

        Self {
            sample_rate_hz,
            pilot_lp_alpha,
            pilot_lp_alpha_side: 1.0
                - (1.0 - pilot_lp_alpha).powi(PILOT_SIDE_UPDATE_INTERVAL as i32),
            pilot_level_alpha: alpha_from_tau(sample_rate_hz, 0.02),
            pilot_power_alpha: alpha_from_tau(sample_rate_hz, 0.02),
            pilot_fraction_alpha: alpha_from_tau(sample_rate_hz, 0.03),
            pilot_quality_alpha: alpha_from_tau(sample_rate_hz, 0.05),
            blend_attack_alpha: alpha_from_tau(sample_rate_hz, 0.03),
            blend_release_alpha: alpha_from_tau(sample_rate_hz, 0.20),
            lr_phase_track_alpha: alpha_from_cutoff(sample_rate_hz, LR_PHASE_TRACK_LPF_HZ),
            deemphasis_alpha,
            pilot_lock_low: 0.010,
            pilot_lock_high: 0.030,
            pilot_fraction_low: 0.006,
            pilot_fraction_high: 0.020,
            pilot_quality_low: 1.8,
            pilot_quality_high: 4.0,
            stereo_lock_on: 0.55,
            stereo_lock_off: 0.35,
        }
    }
}

impl NcoState {
    fn new(sample_rate_hz: f32) -> Self {
        Self {
            pilot: PhaseNco::new(19_000.0, sample_rate_hz),
            pilot_lo: PhaseNco::new(18_000.0, sample_rate_hz),
            pilot_hi: PhaseNco::new(20_000.0, sample_rate_hz),
        }
    }

    fn reset(&mut self) {
        self.pilot.reset();
        self.pilot_lo.reset();
        self.pilot_hi.reset();
    }
}

impl PllState {
    fn new(sample_rate_hz: f32) -> Self {
        let wn = 2.0 * std::f32::consts::PI * PLL_BW_HZ / sample_rate_hz;
        Self {
            phase_err_last: 0.0,
            kp: 2.0 * PLL_DAMPING * wn,
            ki: wn * wn,
            freq_corr: 0.0,
            freq_corr_max: 2.0 * std::f32::consts::PI * PLL_MAX_FREQ_ERR_HZ / sample_rate_hz,
            update_countdown: 0,
            mix_cos2err: 1.0,
            mix_sin2err: 0.0,
        }
    }

    fn reset(&mut self) {
        self.phase_err_last = 0.0;
        self.freq_corr = 0.0;
        self.update_countdown = 0;
        self.mix_cos2err = 1.0;
        self.mix_sin2err = 0.0;
    }
}

impl DcBlockState {
    fn new(sample_rate_hz: f32) -> Self {
        Self {
            prev_x: 0.0,
            prev_y: 0.0,
            hp_a: (-2.0 * std::f32::consts::PI * 30.0 / sample_rate_hz).exp(),
        }
    }

    fn reset(&mut self) {
        self.prev_x = 0.0;
        self.prev_y = 0.0;
    }
}

impl FilterState {
    fn new(sample_rate_hz: f32) -> Self {
        Self {
            sum_lpf: FirFilter::new_lowpass_hamming(
                SUM_AUDIO_FIR_TAPS,
                SUM_AUDIO_LPF_CUTOFF_HZ / sample_rate_hz,
            ),
            diff_lpf: ComplexFirFilter::new_lowpass_hamming(
                DIFF_AUDIO_FIR_TAPS,
                DIFF_AUDIO_LPF_CUTOFF_HZ / sample_rate_hz,
            ),
        }
    }

    fn reset(&mut self) {
        self.sum_lpf.reset();
        self.diff_lpf.reset();
    }
}

impl LrState {
    fn reset(&mut self) {
        self.re_lp = 0.0;
        self.im_lp = 0.0;
        self.corr_cos = 1.0;
        self.corr_sin = 0.0;
        self.update_countdown = 0;
    }
}

impl AudioState {
    fn reset(&mut self) {
        self.deemphasis_l = 0.0;
        self.deemphasis_r = 0.0;
    }
}

impl BlendState {
    fn reset(&mut self) {
        self.stereo_blend = 0.0;
        self.stereo_locked = false;
        self.mono_fallback_count = 0;
    }
}

impl FMStereoDecoder {
    pub fn new(sample_rate_hz: f32, deemphasis_tau_us: Option<f32>) -> Self {
        assert!(sample_rate_hz > 0.0, "sample_rate_hz must be > 0");
        let processing_rate_hz = sample_rate_hz.min(FM_STEREO_INTERMEDIATE_RATE_HZ);
        let cfg = FMStereoConfig::new(processing_rate_hz, deemphasis_tau_us);
        let mpx_resampler = if sample_rate_hz.round() as u32 > processing_rate_hz.round() as u32 {
            Some(Resampler::new_with_cutoff(
                sample_rate_hz.round() as u32,
                processing_rate_hz.round() as u32,
                Some(FM_STEREO_MPX_RESAMPLE_CUTOFF_HZ),
            ))
        } else {
            None
        };
        let mut lr = LrState::default();
        lr.reset();

        Self {
            cfg,
            mpx_input: MpxInputState {
                resampler: mpx_resampler,
                buffer: Vec::with_capacity(8_192),
            },
            nco: NcoState::new(processing_rate_hz),
            pilot: PilotState::default(),
            pll: PllState::new(processing_rate_hz),
            dc: DcBlockState::new(processing_rate_hz),
            filters: FilterState::new(processing_rate_hz),
            lr,
            audio: AudioState::default(),
            blend: BlendState::default(),
        }
    }

    pub fn reset(&mut self) {
        if let Some(resampler) = self.mpx_input.resampler.as_mut() {
            resampler.reconfigure(
                resampler.source_rate,
                resampler.target_rate,
                Some(FM_STEREO_MPX_RESAMPLE_CUTOFF_HZ),
            );
        }
        self.mpx_input.buffer.clear();
        self.nco.reset();
        self.pilot = PilotState::default();
        self.pll.reset();
        self.dc.reset();
        self.filters.reset();
        self.lr.reset();
        self.audio.reset();
        self.blend.reset();
    }

    pub fn process(&mut self, mpx: &[f32], left: &mut Vec<f32>, right: &mut Vec<f32>) {
        if mpx.is_empty() {
            return;
        }
        let mut taken = None;
        if let Some(resampler) = self.mpx_input.resampler.as_mut() {
            self.mpx_input.buffer.clear();
            self.mpx_input.buffer.reserve(
                ((mpx.len() as f32 / resampler.source_rate as f32) * resampler.target_rate as f32 * 1.5)
                    as usize,
            );
            resampler.process(mpx, &mut self.mpx_input.buffer);
            taken = Some(std::mem::take(&mut self.mpx_input.buffer));
        }
        let mpx_input = taken.as_deref().unwrap_or(mpx);
        self.process_core(mpx_input, left, right);
        if let Some(mut v) = taken {
            v.clear();
            self.mpx_input.buffer = v;
        }
    }

    pub fn processing_sample_rate_hz(&self) -> f32 {
        self.cfg.sample_rate_hz
    }

    fn process_core(&mut self, mpx: &[f32], left: &mut Vec<f32>, right: &mut Vec<f32>) {
        left.clear();
        right.clear();
        left.reserve(mpx.len());
        right.reserve(mpx.len());
        let (mut s19, mut c19) = self.nco.pilot.sin_cos();

        for &raw in mpx {
            // 1) MPX低域DCを除去
            let x = self.process_dc(raw);

            // 2) pilotの同期検波と品質推定を更新
            self.update_pilot_tracking(x, s19, c19);

            // 3) pilot位相誤差でPLLを更新
            self.update_pll();

            // 4) pilot品質からstereo blend/lockを更新
            self.update_blend_and_lock();

            // 5) 38k同期検波とL-R位相補正
            let (sum, lr_aligned) = self.extract_sum_and_lr(x, s19, c19);

            // 6) L/R合成とdeemphasis
            let (l, r) = self.mix_and_postprocess(sum, lr_aligned);

            left.push(l);
            right.push(r);

            let loop_term = self.pll.freq_corr + self.pll.kp * self.pll.phase_err_last;
            (s19, c19) = self.nco.pilot.sin_cos_and_advance(loop_term);
            self.nco.pilot_lo.advance(loop_term);
            self.nco.pilot_hi.advance(loop_term);
        }
    }

    /// MPXの低域DCを軽く除去する。
    #[inline(always)]
    fn process_dc(&mut self, raw: f32) -> f32 {
        let x = raw - self.dc.prev_x + self.dc.hp_a * self.dc.prev_y;
        self.dc.prev_x = raw;
        self.dc.prev_y = x;
        x
    }

    /// 19k pilot同期検波と、18k/20k側帯域によるノイズ床推定を更新する。
    #[inline(always)]
    fn update_pilot_tracking(&mut self, x: f32, s19: f32, c19: f32) {
        let pilot_i = x * c19;
        let pilot_q = -x * s19;
        let pilot_mix_power_inst = pilot_i * pilot_i + pilot_q * pilot_q;
        self.pilot.mix_power += self.cfg.pilot_power_alpha * (pilot_mix_power_inst - self.pilot.mix_power);
        self.pilot.i_lp += self.cfg.pilot_lp_alpha * (pilot_i - self.pilot.i_lp);
        self.pilot.q_lp += self.cfg.pilot_lp_alpha * (pilot_q - self.pilot.q_lp);

        if self.pilot.side_update_countdown == 0 {
            let (s18, c18) = self.nco.pilot_lo.sin_cos();
            let (s20, c20) = self.nco.pilot_hi.sin_cos();
            let pilot_i_lo = x * c18;
            let pilot_q_lo = -x * s18;
            let pilot_i_hi = x * c20;
            let pilot_q_hi = -x * s20;
            self.pilot.i_lo_lp += self.cfg.pilot_lp_alpha_side * (pilot_i_lo - self.pilot.i_lo_lp);
            self.pilot.q_lo_lp += self.cfg.pilot_lp_alpha_side * (pilot_q_lo - self.pilot.q_lo_lp);
            self.pilot.i_hi_lp += self.cfg.pilot_lp_alpha_side * (pilot_i_hi - self.pilot.i_hi_lp);
            self.pilot.q_hi_lp += self.cfg.pilot_lp_alpha_side * (pilot_q_hi - self.pilot.q_hi_lp);
            self.pilot.side_update_countdown = PILOT_SIDE_UPDATE_INTERVAL - 1;
        } else {
            self.pilot.side_update_countdown -= 1;
        }
    }

    /// 位相誤差を計算し、PLLの周波数補正値を更新する。
    #[inline(always)]
    fn update_pll(&mut self) {
        if self.pll.update_countdown == 0 {
            let pilot_phase_err = pilot_phase_error_from_iq(self.pilot.i_lp, self.pilot.q_lp);
            self.pll.phase_err_last = pilot_phase_err;
            let (sin2err, cos2err) = (2.0 * pilot_phase_err).sin_cos();
            self.pll.mix_cos2err = cos2err;
            self.pll.mix_sin2err = sin2err;
            self.pll.freq_corr = (self.pll.freq_corr + self.pll.ki * pilot_phase_err)
                .clamp(-self.pll.freq_corr_max, self.pll.freq_corr_max);
            self.pll.update_countdown = PLL_UPDATE_INTERVAL - 1;
        } else {
            self.pll.update_countdown -= 1;
        }
    }

    /// pilotの強度/純度からblend値とlock状態を更新する。
    #[inline(always)]
    fn update_blend_and_lock(&mut self) {
        let pilot_coherent_power = self.pilot.i_lp * self.pilot.i_lp + self.pilot.q_lp * self.pilot.q_lp;
        let pilot_side_power = 0.5
            * ((self.pilot.i_lo_lp * self.pilot.i_lo_lp + self.pilot.q_lo_lp * self.pilot.q_lo_lp)
                + (self.pilot.i_hi_lp * self.pilot.i_hi_lp + self.pilot.q_hi_lp * self.pilot.q_hi_lp));
        let pilot_level_inst = pilot_coherent_power.sqrt() * 2.0;
        self.pilot.level += self.cfg.pilot_level_alpha * (pilot_level_inst - self.pilot.level);
        let pilot_fraction_inst = pilot_coherent_power / (self.pilot.mix_power + 1e-9);
        self.pilot.fraction += self.cfg.pilot_fraction_alpha * (pilot_fraction_inst - self.pilot.fraction);
        let pilot_quality_inst = pilot_coherent_power / (pilot_side_power + 1e-9);
        self.pilot.quality += self.cfg.pilot_quality_alpha * (pilot_quality_inst - self.pilot.quality);

        let level_denom = (self.cfg.pilot_lock_high - self.cfg.pilot_lock_low).max(1e-6);
        let frac_denom = (self.cfg.pilot_fraction_high - self.cfg.pilot_fraction_low).max(1e-6);
        let level_gate = clamp01((self.pilot.level - self.cfg.pilot_lock_low) / level_denom);
        let frac_gate = clamp01((self.pilot.fraction - self.cfg.pilot_fraction_low) / frac_denom);
        let quality_denom = (self.cfg.pilot_quality_high - self.cfg.pilot_quality_low).max(1e-6);
        let quality_gate = clamp01((self.pilot.quality - self.cfg.pilot_quality_low) / quality_denom);
        let target_blend = level_gate * quality_gate * frac_gate;
        let blend_alpha = if target_blend > self.blend.stereo_blend {
            self.cfg.blend_attack_alpha
        } else {
            self.cfg.blend_release_alpha
        };
        self.blend.stereo_blend += blend_alpha * (target_blend - self.blend.stereo_blend);

        let locked_now = if self.blend.stereo_locked {
            self.blend.stereo_blend >= self.cfg.stereo_lock_off
        } else {
            self.blend.stereo_blend >= self.cfg.stereo_lock_on
        };
        if self.blend.stereo_locked && !locked_now {
            self.blend.mono_fallback_count = self.blend.mono_fallback_count.saturating_add(1);
        }
        self.blend.stereo_locked = locked_now;
    }

    /// 38k同期検波でL-Rをベースバンド化し、L+RとL-Rを抽出する。
    #[inline(always)]
    fn extract_sum_and_lr(&mut self, x: f32, s19: f32, c19: f32) -> (f32, f32) {
        let cos2phi = c19 * c19 - s19 * s19;
        let sin2phi = 2.0 * s19 * c19;
        let c38 = cos2phi * self.pll.mix_cos2err - sin2phi * self.pll.mix_sin2err;
        let s38 = sin2phi * self.pll.mix_cos2err + cos2phi * self.pll.mix_sin2err;
        let lr_i_raw = 2.0 * x * c38;
        let lr_q_raw = -2.0 * x * s38;

        let sum = self.filters.sum_lpf.process_sample(x);
        let diff = self.filters.diff_lpf.process_sample(Complex::new(lr_i_raw, lr_q_raw));
        let diff_i = diff.re;
        let diff_q = diff.im;

        let lr2_re = diff_i * diff_i - diff_q * diff_q;
        let lr2_im = 2.0 * diff_i * diff_q;
        self.lr.re_lp += self.cfg.lr_phase_track_alpha * (lr2_re - self.lr.re_lp);
        self.lr.im_lp += self.cfg.lr_phase_track_alpha * (lr2_im - self.lr.im_lp);
        if self.lr.update_countdown == 0 {
            let lr_phase_corr = 0.5 * self.lr.im_lp.atan2(self.lr.re_lp);
            let (s_lr, c_lr) = lr_phase_corr.sin_cos();
            self.lr.corr_cos = c_lr;
            self.lr.corr_sin = s_lr;
            self.lr.update_countdown = LR_PHASE_TRACK_UPDATE_INTERVAL - 1;
        } else {
            self.lr.update_countdown -= 1;
        }
        let lr_aligned = diff_i * self.lr.corr_cos + diff_q * self.lr.corr_sin;
        (sum, lr_aligned)
    }

    /// L/R合成後、必要ならdeemphasisを適用する。
    #[inline(always)]
    fn mix_and_postprocess(&mut self, sum: f32, lr_aligned: f32) -> (f32, f32) {
        let lr = lr_aligned * self.blend.stereo_blend;
        let mut l = sum + lr;
        let mut r = sum - lr;

        if let Some(alpha) = self.cfg.deemphasis_alpha {
            self.audio.deemphasis_l += alpha * (l - self.audio.deemphasis_l);
            self.audio.deemphasis_r += alpha * (r - self.audio.deemphasis_r);
            l = self.audio.deemphasis_l;
            r = self.audio.deemphasis_r;
        }
        (l, r)
    }

    pub fn stats(&self) -> FMStereoStats {
        let i_abs = self.pilot.i_lp.abs();
        let q_abs = self.pilot.q_lp.abs();
        let q_over_i = q_abs / (i_abs + 1e-9);
        let phase_err_abs = self.pll.phase_err_last.abs();
        let pll_locked = self.pilot.level >= self.cfg.pilot_lock_low
            && phase_err_abs < PLL_LOCK_ERR_RAD
            && q_over_i < PLL_LOCK_Q_OVER_I_MAX;
        let pll_freq_corr_hz = self.pll.freq_corr * self.cfg.sample_rate_hz / (2.0 * std::f32::consts::PI);

        FMStereoStats {
            pilot_level: self.pilot.level,
            stereo_blend: self.blend.stereo_blend,
            stereo_locked: self.blend.stereo_locked,
            mono_fallback_count: self.blend.mono_fallback_count,
            pll_phase_err_rad: self.pll.phase_err_last,
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
            let dsb = lr * (2.0 * std::f32::consts::PI * 38_000.0 * t + 2.0 * pilot_phase).cos();
            mpx.push(0.45 * lp + pilot + 0.45 * dsb);
        }
        mpx
    }

    fn build_stereo_mpx_with_lr_phase_mismatch(
        fs: f32,
        left: &[f32],
        right: &[f32],
        pilot_phase: f32,
        lr_extra_phase: f32,
    ) -> Vec<f32> {
        assert_eq!(left.len(), right.len());
        let mut mpx = Vec::with_capacity(left.len());
        for i in 0..left.len() {
            let t = i as f32 / fs;
            let lp = left[i] + right[i];
            let lr = left[i] - right[i];
            let pilot = 0.10 * (2.0 * std::f32::consts::PI * 19_000.0 * t + pilot_phase).cos();
            let dsb = lr
                * (2.0 * std::f32::consts::PI * 38_000.0 * t + 2.0 * pilot_phase + lr_extra_phase)
                    .cos();
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

    fn tone_amp(signal: &[f32], fs_hz: f32, freq_hz: f32) -> f32 {
        if signal.is_empty() {
            return 0.0;
        }
        let w = 2.0 * std::f32::consts::PI * freq_hz / fs_hz;
        let mut re = 0.0f32;
        let mut im = 0.0f32;
        for (n, &x) in signal.iter().enumerate() {
            let phi = w * n as f32;
            re += x * phi.cos();
            im -= x * phi.sin();
        }
        (re * re + im * im).sqrt() / signal.len() as f32
    }

    fn samples_for_seconds(fs: f32, sec: f32) -> usize {
        (fs * sec).ceil() as usize
    }

    fn required_lock_samples(fs: f32) -> usize {
        // 根拠:
        // - pilot_quality の時定数: 0.05s (5τで約99.3%収束 -> 0.25s)
        // - PLL 2次系: Ts(2%) ≈ 4/(ζωn), ωn=2π*PLL_BW_HZ
        // - blend attack: τ=0.03s, 0->0.8 到達時間 -τln(1-0.8)
        // これらの最大に安全係数を掛ける。
        let pll_settle_sec = 4.0 / (PLL_DAMPING * 2.0 * std::f32::consts::PI * PLL_BW_HZ);
        let pilot_quality_settle_sec = 5.0 * 0.05;
        let blend_to_0p8_sec = -0.03 * (1.0 - 0.8f32).ln();
        let need_sec = pll_settle_sec
            .max(pilot_quality_settle_sec)
            .max(blend_to_0p8_sec)
            * 1.2;
        samples_for_seconds(fs, need_sec)
    }

    fn required_unlock_samples(fs: f32) -> usize {
        // 根拠:
        // - blend release の時定数: 0.20s
        // - 1.0 -> 0.1 へ減衰: t = -τ ln(0.1) ≈ 0.4605s
        // - pilot品質側の減衰(0.05s*5)より release が支配的
        let blend_to_0p1_sec = -0.20 * 0.1f32.ln();
        let pilot_quality_decay_sec = 5.0 * 0.05;
        let need_sec = blend_to_0p1_sec.max(pilot_quality_decay_sec) * 1.1;
        samples_for_seconds(fs, need_sec)
    }

    fn analysis_window_samples(fs: f32) -> usize {
        // 分離係数推定の安定化用窓（約150ms）
        samples_for_seconds(fs, 0.15)
    }

    #[test]
    fn duration_helpers_cover_lock_unlock_dynamics() {
        let fs = 200_000.0f32;
        let lock_samples = required_lock_samples(fs);
        let unlock_samples = required_unlock_samples(fs);
        let analysis_samples = analysis_window_samples(fs);

        let pll_settle_sec = 4.0 / (PLL_DAMPING * 2.0 * std::f32::consts::PI * PLL_BW_HZ);
        let pilot_quality_settle_sec = 5.0 * 0.05;
        let blend_attack_to_0p8_sec = -0.03 * (1.0 - 0.8f32).ln();
        let unlock_to_0p1_sec = -0.20 * 0.1f32.ln();

        assert!(
            lock_samples >= samples_for_seconds(fs, pll_settle_sec),
            "lock window too short for PLL settle: lock_samples={} pll_settle_sec={}",
            lock_samples,
            pll_settle_sec
        );
        assert!(
            lock_samples >= samples_for_seconds(fs, pilot_quality_settle_sec),
            "lock window too short for pilot quality settle: lock_samples={} sec={}",
            lock_samples,
            pilot_quality_settle_sec
        );
        assert!(
            lock_samples >= samples_for_seconds(fs, blend_attack_to_0p8_sec),
            "lock window too short for blend attack settle: lock_samples={} sec={}",
            lock_samples,
            blend_attack_to_0p8_sec
        );
        assert!(
            unlock_samples >= samples_for_seconds(fs, unlock_to_0p1_sec),
            "unlock window too short for blend release settle: unlock_samples={} sec={}",
            unlock_samples,
            unlock_to_0p1_sec
        );
        assert!(
            analysis_samples >= samples_for_seconds(fs, 0.10),
            "analysis window too short: analysis_samples={}",
            analysis_samples
        );
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
        let n = required_lock_samples(fs);
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
            let corr = dec.pll.freq_corr;
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
        let n = required_lock_samples(fs);
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

        let i_abs = dec.pilot.i_lp.abs();
        let q_abs = dec.pilot.q_lp.abs();
        let q_over_i = q_abs / (i_abs + 1e-9);
        assert!(
            q_over_i < 0.35,
            "PLL did not align pilot on I-axis: i_lp={} q_lp={} q_over_i={}",
            dec.pilot.i_lp,
            dec.pilot.q_lp,
            q_over_i
        );
        assert!(
            dec.pll.phase_err_last.abs() < 0.35,
            "PLL phase error did not converge near zero: err={}",
            dec.pll.phase_err_last
        );
    }

    #[test]
    fn stereo_separation_is_stable_with_pilot_phase_offset() {
        let fs = 200_000.0f32;
        let settle = required_lock_samples(fs);
        let n = settle + analysis_window_samples(fs);
        let pilot_phase = 1.1f32;
        let left_src = build_program_signal(fs, n, &[700.0, 1_300.0, 2_100.0, 3_700.0]);
        let right_src = build_program_signal(fs, n, &[900.0, 1_700.0, 2_900.0, 4_300.0]);
        let mpx = build_stereo_mpx_from_program_with_phase(fs, &left_src, &right_src, pilot_phase);

        let mut dec = FMStereoDecoder::new(fs, None);
        let mut l_out = Vec::new();
        let mut r_out = Vec::new();
        dec.process(&mpx, &mut l_out, &mut r_out);

        let skip = settle;
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
        let settle = required_lock_samples(fs);
        let n = settle + analysis_window_samples(fs);
        let left_src = build_program_signal(fs, n, &[700.0, 1_300.0, 2_100.0, 3_700.0]);
        let right_src = build_program_signal(fs, n, &[900.0, 1_700.0, 2_900.0, 4_300.0]);
        let mpx = build_stereo_mpx_from_program(fs, &left_src, &right_src);

        let mut dec = FMStereoDecoder::new(fs, None);
        let mut l_out = Vec::new();
        let mut r_out = Vec::new();
        dec.process(&mpx, &mut l_out, &mut r_out);

        let skip = settle;
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
        assert!(
            st.stereo_blend > 0.5,
            "stereo blend did not rise enough: {}",
            st.stereo_blend
        );
        assert!(st.stereo_locked, "stereo did not lock");
    }

    #[test]
    fn lr_phase_mismatch_keeps_separation_and_level() {
        let fs = 200_000.0f32;
        let settle = required_lock_samples(fs);
        let n = settle + analysis_window_samples(fs);
        let tone_hz = 1_000.0f32;
        let pilot_phase = 0.7f32;
        let lr_extra_phase = 1.1f32; // 約63度。I経路のみだと大きく減衰するケース。

        let mut left_src = vec![0.0f32; n];
        let right_src = vec![0.0f32; n];
        for i in 0..n {
            let t = i as f32 / fs;
            left_src[i] = 0.6 * (2.0 * std::f32::consts::PI * tone_hz * t).sin();
        }

        let mpx_ref =
            build_stereo_mpx_from_program_with_phase(fs, &left_src, &right_src, pilot_phase);
        let mpx_mismatch = build_stereo_mpx_with_lr_phase_mismatch(
            fs,
            &left_src,
            &right_src,
            pilot_phase,
            lr_extra_phase,
        );

        let mut dec = FMStereoDecoder::new(fs, None);
        let mut l_ref = Vec::new();
        let mut r_ref = Vec::new();
        dec.process(&mpx_ref, &mut l_ref, &mut r_ref);

        dec.reset();
        let mut l_mis = Vec::new();
        let mut r_mis = Vec::new();
        dec.process(&mpx_mismatch, &mut l_mis, &mut r_mis);

        let skip = settle;
        let l_ref_amp = tone_amp(&l_ref[skip..], fs, tone_hz);
        let l_mis_amp = tone_amp(&l_mis[skip..], fs, tone_hz);
        let r_mis_amp = tone_amp(&r_mis[skip..], fs, tone_hz);

        let rel = l_mis_amp / (l_ref_amp + 1e-9);
        let sep_db = ratio_db(l_mis_amp, r_mis_amp);

        assert!(
            rel > 0.75,
            "LR phase mismatch attenuated too much: rel={} l_ref={} l_mis={}",
            rel,
            l_ref_amp,
            l_mis_amp
        );
        assert!(
            sep_db > 12.0,
            "LR phase mismatch separation too low: sep_db={} l_mis={} r_mis={}",
            sep_db,
            l_mis_amp,
            r_mis_amp
        );
    }

    #[test]
    fn clean_stereo_reaches_high_blend() {
        let fs = 200_000.0f32;
        let mpx = build_stereo_mpx(fs, required_lock_samples(fs));
        let mut dec = FMStereoDecoder::new(fs, None);
        let mut l = Vec::new();
        let mut r = Vec::new();
        dec.process(&mpx, &mut l, &mut r);

        let st = dec.stats();
        assert!(
            st.stereo_locked,
            "stereo should lock on clean multiplex: {:?}",
            st
        );
        assert!(
            st.stereo_blend > 0.75,
            "stereo blend too low on clean multiplex: {:?}",
            st
        );
    }

    #[test]
    fn mono_program_with_pilot_keeps_lr_difference_low() {
        let fs = 200_000.0f32;
        let settle = required_lock_samples(fs);
        let mpx = build_mono_with_pilot_mpx(fs, settle + analysis_window_samples(fs), 0.01);

        let mut dec = FMStereoDecoder::new(fs, None);
        let mut l_out = Vec::new();
        let mut r_out = Vec::new();
        dec.process(&mpx, &mut l_out, &mut r_out);

        let skip = settle;
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
        let settle = required_lock_samples(fs);
        let mpx = build_mono_with_pilot_mpx(fs, settle + analysis_window_samples(fs), 0.05);

        let mut dec = FMStereoDecoder::new(fs, None);
        let mut l_out = Vec::new();
        let mut r_out = Vec::new();
        dec.process(&mpx, &mut l_out, &mut r_out);

        let skip = settle;
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
        let n = required_lock_samples(fs);

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
        assert!(
            st2.mono_fallback_count >= 1,
            "fallback count did not increment"
        );
        assert!(
            st2.stereo_blend < st1.stereo_blend,
            "blend should decay after pilot loss"
        );
    }

    #[test]
    fn does_not_lock_without_pilot_on_single_tone() {
        let fs = 200_000.0f32;
        let mut dec = FMStereoDecoder::new(fs, None);
        let n = required_lock_samples(fs);

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
        let n = required_lock_samples(fs);

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
        // このテストは「seed差で誤ロックしない」ことの確認が目的。
        // 全seed総当たりは実行時間が重すぎるため、固定代表seedに絞る。
        for seed in [1u32, 3u32, 7u32] {
            let mut dec = FMStereoDecoder::new(fs, None);
            let noise = build_noise(required_lock_samples(fs), 0x1000_0000u32.wrapping_add(seed), 0.9);
            let mut l = Vec::new();
            let mut r = Vec::new();
            let mut peak_blend = 0.0f32;

            for chunk in noise.chunks(8192) {
                dec.process(chunk, &mut l, &mut r);
                peak_blend = peak_blend.max(dec.stats().stereo_blend);
            }

            let st = dec.stats();
            assert!(
                !st.stereo_locked,
                "decoder locked on chunked noise (seed={}): {:?}",
                seed, st
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
        let lock_input = build_stereo_mpx(fs, required_lock_samples(fs));
        let n = analysis_window_samples(fs);

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
        assert!(
            st_lock.stereo_locked,
            "decoder did not lock before pilot-only segment: {:?}",
            st_lock
        );

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

        let lock_input = build_stereo_mpx(fs, required_lock_samples(fs));
        let no_signal = vec![0.0f32; required_unlock_samples(fs)];

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

        let lock_input = build_stereo_mpx(fs, required_lock_samples(fs));
        let no_signal = vec![0.0f32; required_unlock_samples(fs)];
        let mut l = Vec::new();
        let mut r = Vec::new();

        for chunk in lock_input.chunks(8192) {
            dec.process(chunk, &mut l, &mut r);
        }
        let st_lock = dec.stats();
        assert!(
            st_lock.stereo_locked,
            "decoder did not lock in chunked mode before retune: {:?}",
            st_lock
        );

        for chunk in no_signal.chunks(8192) {
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
