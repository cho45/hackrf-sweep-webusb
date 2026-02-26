#[derive(Clone, Copy, Debug, Default)]
pub struct FMStereoStats {
    pub pilot_level: f32,
    pub stereo_blend: f32,
    pub stereo_locked: bool,
    pub mono_fallback_count: u32,
}

/// FM MPX から L/R を復元する簡易ステレオデコーダ。
///
/// - pilot(19kHz) は複素同期検波で位相を追従
/// - L-R は 38kHz 同期検波 + LPF
/// - pilot レベルに応じて stereo blend を自動調整し、ロック不十分時は mono に寄せる
pub struct FMStereoDecoder {
    pilot_phase: f32,
    pilot_omega: f32,

    pilot_i_lp: f32,
    pilot_q_lp: f32,
    pilot_lp_alpha: f32,

    dc_prev_x: f32,
    dc_prev_y: f32,
    dc_hp_a: f32,

    sum_lp: f32,
    diff_lp: f32,
    audio_lp_alpha: f32,

    deemphasis_alpha: Option<f32>,
    deemphasis_l: f32,
    deemphasis_r: f32,

    pilot_level: f32,
    pilot_level_alpha: f32,

    stereo_blend: f32,
    blend_attack_alpha: f32,
    blend_release_alpha: f32,

    pilot_lock_low: f32,
    pilot_lock_high: f32,

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

impl FMStereoDecoder {
    pub fn new(sample_rate_hz: f32, deemphasis_tau_us: Option<f32>) -> Self {
        assert!(sample_rate_hz > 0.0, "sample_rate_hz must be > 0");

        let pilot_omega = 2.0 * std::f32::consts::PI * 19_000.0 / sample_rate_hz;
        let pilot_lp_alpha = alpha_from_cutoff(sample_rate_hz, 250.0);
        let dc_hp_a = (-2.0 * std::f32::consts::PI * 30.0 / sample_rate_hz).exp();
        let audio_lp_alpha = alpha_from_cutoff(sample_rate_hz, 15_000.0);
        let pilot_level_alpha = alpha_from_tau(sample_rate_hz, 0.02);
        let blend_attack_alpha = alpha_from_tau(sample_rate_hz, 0.03);
        let blend_release_alpha = alpha_from_tau(sample_rate_hz, 0.20);
        let deemphasis_alpha = deemphasis_tau_us.and_then(|tau_us| {
            if tau_us <= 0.0 {
                return None;
            }
            Some(alpha_from_tau(sample_rate_hz, tau_us * 1e-6))
        });

        Self {
            pilot_phase: 0.0,
            pilot_omega,
            pilot_i_lp: 0.0,
            pilot_q_lp: 0.0,
            pilot_lp_alpha,
            dc_prev_x: 0.0,
            dc_prev_y: 0.0,
            dc_hp_a,
            sum_lp: 0.0,
            diff_lp: 0.0,
            audio_lp_alpha,
            deemphasis_alpha,
            deemphasis_l: 0.0,
            deemphasis_r: 0.0,
            pilot_level: 0.0,
            pilot_level_alpha,
            stereo_blend: 0.0,
            blend_attack_alpha,
            blend_release_alpha,
            pilot_lock_low: 0.010,
            pilot_lock_high: 0.030,
            stereo_locked: false,
            mono_fallback_count: 0,
        }
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
            let pilot_i = x * c19;
            let pilot_q = -x * s19;
            self.pilot_i_lp += self.pilot_lp_alpha * (pilot_i - self.pilot_i_lp);
            self.pilot_q_lp += self.pilot_lp_alpha * (pilot_q - self.pilot_q_lp);

            let pilot_phase_err = self.pilot_q_lp.atan2(self.pilot_i_lp);
            let pilot_level_inst = (self.pilot_i_lp * self.pilot_i_lp + self.pilot_q_lp * self.pilot_q_lp).sqrt() * 2.0;
            self.pilot_level += self.pilot_level_alpha * (pilot_level_inst - self.pilot_level);

            let denom = (self.pilot_lock_high - self.pilot_lock_low).max(1e-6);
            let target_blend = clamp01((self.pilot_level - self.pilot_lock_low) / denom);
            let blend_alpha = if target_blend > self.stereo_blend {
                self.blend_attack_alpha
            } else {
                self.blend_release_alpha
            };
            self.stereo_blend += blend_alpha * (target_blend - self.stereo_blend);

            let locked_now = self.stereo_blend >= 0.5;
            if self.stereo_locked && !locked_now {
                self.mono_fallback_count = self.mono_fallback_count.saturating_add(1);
            }
            self.stereo_locked = locked_now;

            let c38 = (2.0 * (self.pilot_phase + pilot_phase_err)).cos();
            let lr_raw = 2.0 * x * c38;

            self.sum_lp += self.audio_lp_alpha * (x - self.sum_lp);
            self.diff_lp += self.audio_lp_alpha * (lr_raw - self.diff_lp);

            let lr = self.diff_lp * self.stereo_blend;
            let mut l = self.sum_lp + lr;
            let mut r = self.sum_lp - lr;

            if let Some(alpha) = self.deemphasis_alpha {
                self.deemphasis_l += alpha * (l - self.deemphasis_l);
                self.deemphasis_r += alpha * (r - self.deemphasis_r);
                l = self.deemphasis_l;
                r = self.deemphasis_r;
            }

            left.push(l);
            right.push(r);

            self.pilot_phase += self.pilot_omega;
            if self.pilot_phase >= 2.0 * std::f32::consts::PI {
                self.pilot_phase -= 2.0 * std::f32::consts::PI;
            }
        }
    }

    pub fn stats(&self) -> FMStereoStats {
        FMStereoStats {
            pilot_level: self.pilot_level,
            stereo_blend: self.stereo_blend,
            stereo_locked: self.stereo_locked,
            mono_fallback_count: self.mono_fallback_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_amplitude(samples: &[f32], sample_rate: f32, freq_hz: f32) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let mut i_acc = 0.0f32;
        let mut q_acc = 0.0f32;
        for (n, &x) in samples.iter().enumerate() {
            let t = n as f32 / sample_rate;
            let phase = 2.0 * std::f32::consts::PI * freq_hz * t;
            i_acc += x * phase.cos();
            q_acc += x * phase.sin();
        }
        2.0 * (i_acc * i_acc + q_acc * q_acc).sqrt() / samples.len() as f32
    }

    #[test]
    fn stereo_separates_left_and_right_tones() {
        let fs = 200_000.0f32;
        let n = 200_000usize;

        let mut mpx = Vec::with_capacity(n);

        for i in 0..n {
            let t = i as f32 / fs;
            let l = 0.5 * (2.0 * std::f32::consts::PI * 1_000.0 * t).sin();
            let r = 0.5 * (2.0 * std::f32::consts::PI * 2_000.0 * t).sin();
            let lp = l + r;
            let lr = l - r;
            let pilot = 0.10 * (2.0 * std::f32::consts::PI * 19_000.0 * t).cos();
            let dsb = lr * (2.0 * std::f32::consts::PI * 38_000.0 * t).cos();
            mpx.push(0.45 * lp + pilot + 0.45 * dsb);
        }

        let mut dec = FMStereoDecoder::new(fs, None);
        let mut l_out = Vec::new();
        let mut r_out = Vec::new();
        dec.process(&mpx, &mut l_out, &mut r_out);

        let skip = 20_000usize;
        let l = &l_out[skip..];
        let r = &r_out[skip..];

        let l_1k = tone_amplitude(l, fs, 1_000.0);
        let l_2k = tone_amplitude(l, fs, 2_000.0);
        let r_1k = tone_amplitude(r, fs, 1_000.0);
        let r_2k = tone_amplitude(r, fs, 2_000.0);

        assert!(l_1k > l_2k * 2.0, "left ch separation too low: 1k={} 2k={}", l_1k, l_2k);
        assert!(r_2k > r_1k * 2.0, "right ch separation too low: 2k={} 1k={}", r_2k, r_1k);

        let st = dec.stats();
        assert!(st.stereo_blend > 0.5, "stereo blend did not rise enough: {}", st.stereo_blend);
        assert!(st.stereo_locked, "stereo did not lock");
    }

    #[test]
    fn fallback_counter_increments_when_lock_is_lost() {
        let fs = 200_000.0f32;
        let mut dec = FMStereoDecoder::new(fs, None);
        let n = 120_000usize;

        let with_pilot: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / fs;
                0.1 * (2.0 * std::f32::consts::PI * 19_000.0 * t).cos()
            })
            .collect();

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
        assert!(st1.stereo_blend > 0.4, "blend should rise with pilot, got {}", st1.stereo_blend);

        dec.process(&no_pilot, &mut l, &mut r);
        let st2 = dec.stats();
        assert!(st2.mono_fallback_count >= 1, "fallback count did not increment");
        assert!(st2.stereo_blend < st1.stereo_blend, "blend should decay after pilot loss");
    }
}
