use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use std::slice;

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

#[allow(unused_macros)]
macro_rules! console_log {
    // Note that this is using the `log` function imported above during
    // `bare_bones`
    ($($t:tt)*) => (log(&format_args!($($t)*).to_string()))
}

#[wasm_bindgen]
pub fn set_panic_hook() {
    console_error_panic_hook::set_once();
}

#[wasm_bindgen]
pub struct FFT {
    n: usize,
    smoothing_time_constant: f32,
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    prev: Box<[f32]>,
    /// FFT作業用バッファ。再利用してアロケーションを回避
    buffer: Vec<rustfft::num_complex::Complex<f32>>,
    /// スケーリング（1/128 と 1/n）を含めた窓関数
    scaled_window: Box<[f32]>,
}

#[wasm_bindgen]
impl FFT {
    /// 新しいFFTプロセッサを作成する。
    ///
    /// # 引数
    /// * `n` - FFTサイズ。2の累乗であり、0より大きい必要がある
    /// * `window_` - 窓関数の配列。長さは `n` と等しくなければならない
    ///
    /// # パニック
    /// * `n` が 0 の場合
    /// * `n` が 2の累乗でない場合
    /// * `window_.len() != n` の場合
    #[allow(clippy::new_without_default)]
    #[wasm_bindgen(constructor)]
    pub fn new(n: usize, window_: &[f32]) -> Self {
        assert!(n > 0, "FFT size must be positive, got {}", n);
        assert!(n.is_power_of_two(), "FFT size must be a power of two, got {}", n);
        assert_eq!(window_.len(), n, "Window size must match FFT size (expected {}, got {})", n, window_.len());

        let fft = FftPlanner::new().plan_fft_forward(n);
        let prev = vec![0.0; n].into_boxed_slice();
        let smoothing_time_constant = 0.0;
        let buffer = vec![Complex { re: 0.0, im: 0.0 }; n];

        // 窓関数にスケーリング係数を事前に適用しておく
        // 1/128: i8 (-128..127) を -1..1 に正規化
        // 1/n: FFTの正規化
        let scale = 1.0 / (128.0 * n as f32);
        let scaled_window = window_.iter().map(|&w| w * scale).collect::<Vec<_>>().into_boxed_slice();

        FFT {
            n,
            smoothing_time_constant,
            fft,
            prev,
            buffer,
            scaled_window,
        }
    }

    pub fn set_smoothing_time_constant(&mut self, val: f32) {
        self.smoothing_time_constant = val;
    }

    pub fn get_n(&self) -> usize {
        self.n
    }

    /// HackRF One の IQ サンプル列に対して複素 FFT を実行し、
    /// スペクトログラムのウォーターフォール表示に必要な前処理を全て行う。
    ///
    /// このメソッドは以下の処理をワンパスで実行する：
    /// 1. IQ サンプルの正規化（i8 → f32）
    /// 2. 窓関数の適用
    /// 3. 複素 FFT
    /// 4. DC 中心配置への周波数軸の並べ替え
    /// 5. 指数移動平均によるスムージング（設定時）
    /// 6. dB スケールへの変換
    ///
    /// 出力された配列は、そのままスペクトログラムの1行（時刻 t におけるスペクトル）として
    /// ウォーターフォール表示に使用できる。
    ///
    /// # 入力形式
    /// * `input_` - i8の配列として表現された複素数列 `[re0, im0, re1, im1, ...]`
    ///               長さは `self.n * 2` でなければならない
    ///
    /// # 出力形式
    /// * `result` - 結果を格納するバッファ。長さは `self.n` でなければならない
    ///   - `result[0 .. half_n]` - 負の周波数成分（DC中心配置、dBスケール）
    ///   - `result[half_n .. n]` - 正の周波数成分（DC中心配置、dBスケール）
    ///
    /// # コントラクト（呼び出し側の責任）
    /// * `input_.len() == self.n * 2` でなければならない
    /// * `result.len() == self.n` でなければならない
    ///
    /// # 安全性
    /// この関数は unsafe なメモリ再解釈を使用する。コントラクトに違反する場合、
    /// 未定義動作を引き起こす可能性がある。
    pub fn fft(&mut self, input_: &[i8], result: &mut [f32]) {
        debug_assert_eq!(input_.len(), self.n * 2, "Input length must be n * 2");
        debug_assert_eq!(result.len(), self.n, "Result length must be n");

        // i8配列 [re0, im0, re1, im1, ...] を Complex<i8> スライスとして再解釈
        let input_complex: &[Complex<i8>] = unsafe {
            slice::from_raw_parts(input_.as_ptr() as *const Complex<i8>, self.n)
        };

        // 作業用バッファ（構造体に保持して再利用、アロケーション回避）
        let buffer = &mut self.buffer;

        // 正規化と窓関数の適用。scaled_window に 1/128 と 1/n のスケールが含まれている。
        for i in 0..self.n {
            buffer[i] = Complex {
                re: input_complex[i].re as f32,
                im: input_complex[i].im as f32,
            } * self.scaled_window[i];
        }

        // FFT実行（in-place変換）
        self.fft.process(buffer);

        // 以下の処理を1パスに統合：
        // 1. DC中心配置への再配置
        // 2. 指数移動平均によるスムージング
        // 3. dBスケールへの変換
        let half_n = self.n / 2;
        let alpha = self.smoothing_time_constant;
        let inv_alpha = 1.0 - alpha;

        for i in 0..self.n {
            // result[i] に入れるべき成分の、buffer内でのインデックスを計算（DC Shift）
            let src_idx = if i < half_n { i + half_n } else { i - half_n };
            
            // すでに scaled_window により 1/n 倍されているため、norm() するだけでよい
            let magnitude = buffer[src_idx].norm();

            let smoothed = if alpha > 0.0 {
                let s = alpha * self.prev[i] + inv_alpha * magnitude;
                self.prev[i] = s;
                s
            } else {
                magnitude
            };

            // log10(0) = -inf を避けるため、小さな値で下限を設ける
            result[i] = smoothed.max(1e-10).log10() * 10.0;
        }
    }
}

// ============================================================================
// Rust Native Tests
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    /// 窓関数なしの単位窓を生成
    fn ones_window(n: usize) -> Vec<f32> {
        vec![1.0; n]
    }

    #[test]
    fn test_fft_construction() {
        let n = 8;
        let window = ones_window(n);
        let fft = FFT::new(n, &window);

        assert_eq!(fft.n, n);
        // 内部フィールドは直接アクセスできないが、構築が成功すれば OK
    }

    #[test]
    fn test_fft_set_smoothing_time_constant() {
        let n = 8;
        let window = ones_window(n);
        let mut fft = FFT::new(n, &window);

        fft.set_smoothing_time_constant(0.5);
        // 設定が成功すれば OK（内部フィールドはプライベート）
    }

    #[test]
    fn test_fft_dc_input() {
        // DC 成分のみ（全て同じ値）の入力に対する FFT テスト
        let n = 8;
        let window = ones_window(n);
        let mut fft = FFT::new(n, &window);

        let mut input = vec![0i8; n * 2]; // Complex<i8> なので n * 2
        for i in 0..n {
            input[i * 2] = 64; // 宽数 = 64
            input[i * 2 + 1] = 0; // 虚数 = 0
        }

        let mut result = vec![0.0f32; n];
        fft.fft(&input, &mut result);

        // 結果は DC中心に並べ替えられるため、DC成分は中央（half_n）に来る
        let half_n = n / 2;
        let dc_component = result.iter().enumerate().max_by(|a, b| {
            a.1.partial_cmp(b.1).unwrap()
        });

        // DC成分がインデックス4（half_n）にあるはず
        assert_eq!(dc_component.unwrap().0, half_n);
    }

    #[test]
    fn test_fft_zero_input_should_not_produce_inf() {
        // 全て0の入力: log10(0) = -inf になるべきではない
        let n = 8;
        let window = ones_window(n);
        let mut fft = FFT::new(n, &window);

        let input = vec![0i8; n * 2]; // 全て0

        let mut result = vec![0.0f32; n];
        fft.fft(&input, &mut result);

        // 全ての結果が finite であるべき（inf, -inf, NaN でない）
        for (i, &val) in result.iter().enumerate() {
            assert!(
                val.is_finite(),
                "result[{}] = {} is not finite (zero input should not produce inf)",
                i, val
            );
        }
    }

    #[test]
    fn test_fft_smoothing() {
        // スムージングの効果を数値的に検証
        // smoothing_time_constant = 0.5 のとき:
        // result[k] = 0.5 * prev[k] + 0.5 * current[k]
        let n = 8;
        let window = ones_window(n);
        let mut fft = FFT::new(n, &window);
        fft.set_smoothing_time_constant(0.5);

        let mut input = vec![0i8; n * 2];
        for i in 0..n {
            input[i * 2] = 64; // 宽数 = 64
            input[i * 2 + 1] = 0; // 虚数 = 0
        }

        let mut result1 = vec![0.0f32; n];
        fft.fft(&input, &mut result1);

        let mut result2 = vec![0.0f32; n];
        fft.fft(&input, &mut result2);

        // スムージング適用時、2回目の結果は1回目の結果と異なるはず
        // （prevが0でない値を持っているため）
        let mut differences_found = false;
        for i in 0..n {
            if result1[i].is_finite() && result2[i].is_finite() {
                let diff = (result1[i] - result2[i]).abs();
                // スムージングにより値が変化しているはず（誤差許容1e-6）
                if diff > 1e-6 {
                    differences_found = true;
                }
            }
        }
        assert!(
            differences_found,
            "Smoothing should produce different results on consecutive calls with same input"
        );
    }

    #[test]
    fn test_fft_smoothing_disabled_when_constant_is_zero() {
        // smoothing_time_constant = 0 のときスムージングは無効
        let n = 8;
        let window = ones_window(n);
        let mut fft = FFT::new(n, &window);
        // デフォルトは 0.0

        let mut input = vec![0i8; n * 2];
        for i in 0..n {
            input[i * 2] = 64;
            input[i * 2 + 1] = 0;
        }

        let mut result1 = vec![0.0f32; n];
        fft.fft(&input, &mut result1);

        let mut result2 = vec![0.0f32; n];
        fft.fft(&input, &mut result2);

        // スムージング無効時、同じ入力 → 同じ出力
        for i in 0..n {
            if result1[i].is_finite() && result2[i].is_finite() {
                assert_eq!(
                    result1[i], result2[i],
                    "Without smoothing, same input should produce same output at index {}",
                    i
                );
            }
        }
    }

    #[test]
    fn test_fft_smoothing_edge_cases() {
        // smoothing_time_constant の境界値テスト
        let n = 8;
        let window = ones_window(n);

        // 0.0: スムージング無効（上でテスト済み）

        // 1.0: 完全に前の値を保持（新しい値は無視）
        let mut fft = FFT::new(n, &window);
        fft.set_smoothing_time_constant(1.0);

        let mut input = vec![0i8; n * 2];
        for i in 0..n {
            input[i * 2] = 64;
            input[i * 2 + 1] = 0;
        }

        let mut result1 = vec![0.0f32; n];
        fft.fft(&input, &mut result1);

        let mut result2 = vec![0.0f32; n];
        fft.fft(&input, &mut result2);

        // α=1.0 のとき、result2 は result1 と同じはず（prevを完全に維持）
        for i in 0..n {
            if result1[i].is_finite() && result2[i].is_finite() {
                assert_eq!(
                    result1[i], result2[i],
                    "With α=1.0, output should stay constant at index {}",
                    i
                );
            }
        }

        // 負の値: 挙動は未定義だがクラッシュしてはいけない
        let mut fft = FFT::new(n, &window);
        fft.set_smoothing_time_constant(-0.5);
        let mut result = vec![0.0f32; n];
        // クラッシュしなければ OK
        fft.fft(&input, &mut result);

        // 1.0より大きい値: 振動するがクラッシュしてはいけない
        let mut fft = FFT::new(n, &window);
        fft.set_smoothing_time_constant(1.5);
        let mut result = vec![0.0f32; n];
        fft.fft(&input, &mut result);
    }

    #[test]
    fn test_fft_dc_input_magnitude() {
        // DC入力のFFT結果の数値的正しさを検証
        let n = 8;
        let window = ones_window(n);
        let mut fft = FFT::new(n, &window);

        // DC成分: 全て (64 + 0j)
        let mut input = vec![0i8; n * 2];
        for i in 0..n {
            input[i * 2] = 64;
            input[i * 2 + 1] = 0;
        }

        let mut result = vec![0.0f32; n];
        fft.fft(&input, &mut result);

        // 理論値の計算:
        // 入力: 64/128 = 0.5
        // FFT後のDC成分: 0.5 * 8 = 4.0 (norm() で2乗なので 4.0^2 = 16.0、normは sqrt(16) = 4.0)
        // 正規化: 4.0 / 8 = 0.5
        // dB: 10 * log10(0.5) ≈ -3.01
        let half_n = n / 2;
        let dc_value = result[half_n]; // DC成分は中央

        let expected_db = 10.0 * 0.5_f32.log10(); // ≈ -3.01
        assert!(
            (dc_value - expected_db).abs() < 0.1,
            "DC component {} should be close to {} (dB)",
            dc_value, expected_db
        );

        // DC成分が最大であるべき
        let max_idx = result
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        assert_eq!(max_idx, half_n, "DC component should be at index {}", half_n);
    }

    #[test]
    fn test_fft_negative_input() {
        // 負の入力値のテスト
        let n = 8;
        let window = ones_window(n);
        let mut fft = FFT::new(n, &window);

        let mut input = vec![0i8; n * 2];
        for i in 0..n {
            input[i * 2] = -64; // 負の値
            input[i * 2 + 1] = 0;
        }

        let mut result = vec![0.0f32; n];
        fft.fft(&input, &mut result);

        // 全て finite であるべき
        for (i, &val) in result.iter().enumerate() {
            assert!(
                val.is_finite(),
                "result[{}] = {} is not finite (negative input should be handled)",
                i, val
            );
        }
    }

    #[test]
    fn test_fft_i8_boundary_values() {
        // i8 の境界値テスト
        let n = 8;
        let window = ones_window(n);
        let mut fft = FFT::new(n, &window);

        // i8::MIN = -128, i8::MAX = 127
        let test_values = [i8::MIN, -1, 0, 1, i8::MAX];

        for &val in &test_values {
            let mut input = vec![0i8; n * 2];
            for i in 0..n {
                input[i * 2] = val;
                input[i * 2 + 1] = 0;
            }

            let mut result = vec![0.0f32; n];
            fft.fft(&input, &mut result);

            // クラッシュせず、全て finite であるべき
            for (i, &r) in result.iter().enumerate() {
                assert!(
                    r.is_finite(),
                    "result[{}] = {} is not finite for input value {}",
                    i, r, val
                );
            }
        }
    }

    #[test]
    #[should_panic(expected = "Window size must match FFT size")]
    fn test_fft_window_size_mismatch() {
        let n = 8;
        let window = vec![1.0; 4]; // サイズ不足
        let _fft = FFT::new(n, &window);
    }

    #[test]
    #[should_panic(expected = "Window size must match FFT size")]
    fn test_fft_window_size_oversized() {
        let n = 8;
        let window = vec![1.0; 16]; // サイズ超過
        let _fft = FFT::new(n, &window);
    }

    #[test]
    #[should_panic(expected = "FFT size must be positive")]
    fn test_fft_zero_size() {
        let _fft = FFT::new(0, &[]);
    }

    #[test]
    #[should_panic(expected = "FFT size must be a power of two")]
    fn test_fft_non_power_of_two() {
        let n = 7; // 2の累乗でない
        let window = vec![1.0; n];
        let _fft = FFT::new(n, &window);
    }

    #[test]
    #[should_panic(expected = "FFT size must be a power of two")]
    fn test_fft_odd_size() {
        let n = 9; // 奇数
        let window = vec![1.0; n];
        let _fft = FFT::new(n, &window);
    }

    #[test]
    fn test_fft_differential_against_reference() {
        // 参照実装（愚直な実装）と最適化版の結果を比較する
        let n = 16;
        let mut window = vec![0.0f32; n];
        for (i, w) in window.iter_mut().enumerate() {
             // Hann 窓的なものを生成
             *w = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (n - 1) as f32).cos());
        }
        
        let mut fft = FFT::new(n, &window);
        fft.set_smoothing_time_constant(0.3);
        
        let mut input = vec![0i8; n * 2];
        for i in 0..n {
            input[i*2] = (i as i8).wrapping_sub(8).wrapping_mul(10);
            input[i*2+1] = (7i8).wrapping_sub(i as i8).wrapping_mul(10);
        }
        
        // 1回目の実行（prevを0から更新）
        let mut result_opt = vec![0.0f32; n];
        fft.fft(&input, &mut result_opt);
        
        // 参照計算（1回目）
        let mut prev = vec![0.0f32; n]; // 初期状態
        let expected = calculate_reference_fft(n, &window, &input, &mut prev, 0.3);
        
        for i in 0..n {
            assert!((result_opt[i] - expected[i]).abs() < 1e-5, "Mismatch at index {} on 1st run: opt={}, expected={}", i, result_opt[i], expected[i]);
        }
        
        // 2回目の実行（Smoothingの効果を確認）
        fft.fft(&input, &mut result_opt);
        let expected2 = calculate_reference_fft(n, &window, &input, &mut prev, 0.3);
        
        for i in 0..n {
            assert!((result_opt[i] - expected2[i]).abs() < 1e-5, "Mismatch at index {} on 2nd run: opt={}, expected={}", i, result_opt[i], expected2[i]);
        }
    }

    /// 参照用の愚直な計算（効率は無視）
    fn calculate_reference_fft(n: usize, window: &[f32], input: &[i8], prev: &mut [f32], alpha: f32) -> Vec<f32> {
        use rustfft::num_complex::Complex;
        let mut buffer = vec![Complex { re: 0.0, im: 0.0 }; n];
        for i in 0..n {
            buffer[i] = Complex {
                re: input[i*2] as f32 / 128.0,
                im: input[i*2+1] as f32 / 128.0,
            } * window[i];
        }
        
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(n);
        fft.process(&mut buffer);
        
        let half_n = n / 2;
        let mut shifted = vec![0.0f32; n];
        for i in 0..half_n {
            shifted[i + half_n] = buffer[i].norm() / n as f32;
            shifted[i] = buffer[i + half_n].norm() / n as f32;
        }
        
        let mut res = vec![0.0f32; n];
        for i in 0..n {
            let magnitude = if alpha > 0.0 {
                let s = alpha * prev[i] + (1.0 - alpha) * shifted[i];
                prev[i] = s;
                s
            } else {
                shifted[i]
            };
            res[i] = magnitude.max(1e-10).log10() * 10.0;
        }
        res
    }
}

// ============================================================================
// Wasm Tests (wasm-bindgen-test)
// ============================================================================
#[cfg(test)]
mod wasm_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn test_fft_construction_wasm() {
        let n = 8;
        let window = vec![1.0; n];
        let _fft = FFT::new(n, &window);
    }

    #[wasm_bindgen_test]
    fn test_fft_processing_wasm() {
        let n = 8;
        let window = vec![1.0; n];
        let mut fft = FFT::new(n, &window);

        let mut input = vec![0i8; n * 2];
        for i in 0..n {
            input[i * 2] = 64;
            input[i * 2 + 1] = 0;
        }

        let mut result = vec![0.0f32; n];
        fft.fft(&input, &mut result);

        // 結果のサイズが正しいことを確認
        assert_eq!(result.len(), n);
    }
}
