extern crate wasm_bindgen;
extern crate console_error_panic_hook;

//extern crate wee_alloc;
// Use `wee_alloc` as the global allocator.
//#[global_allocator]
//static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

//use std::sync::Arc;
use rustfft::FFTplanner;
use rustfft::num_complex::Complex;
//use rustfft::num_traits::Zero;
//use std::mem;
use std::slice;



use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern {
    pub fn alert(s: &str);

    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);

    #[wasm_bindgen(js_namespace = Math)]
    fn log10(s: f32) -> f32;
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
    fft: std::sync::Arc<dyn rustfft::FFT<f32>>,
    window: Box<[f32]>,
    prev: Box<[f32]>,
}


#[wasm_bindgen]
impl FFT {
    #[allow(clippy::new_without_default)]
    #[wasm_bindgen(constructor)]
    pub fn new(n: usize, window_: &[f32]) -> Self {
        let fft = FFTplanner::new(false).plan_fft(n);
        let mut window = vec![0.0; n].into_boxed_slice();
        window.copy_from_slice(window_);
        let prev = vec![0.0; n].into_boxed_slice();
        let smoothing_time_constant = 0.0;
        FFT {
            n,
            smoothing_time_constant,
            fft,
            window,
            prev
        }
    }

    pub fn set_smoothing_time_constant(&mut self, val: f32) {
        self.smoothing_time_constant = val;
    }

    pub fn fft(&mut self, input_: &mut [i8], result: &mut [f32]) {
        let input_i8:  &mut [Complex<i8>] = unsafe { slice::from_raw_parts_mut(input_  as *mut [i8] as *mut Complex<i8>, self.n )};

        let mut output = Vec::<Complex<f32>>::with_capacity(self.n);
        unsafe { output.set_len(self.n); }

        let mut input = Vec::<Complex<f32>>::with_capacity(self.n);
        unsafe { input.set_len(self.n); }
        for i in 0..self.n {
            input[i] = Complex {
                re: (input_i8[i].re as f32) / 128_f32,
                im: (input_i8[i].im as f32) / 128_f32
            } * self.window[i];
        }

        let half_n = self.n / 2;
        for i in 0..half_n {
            result[i+0] = input[i].re;
            result[i+1] = input[i].im;
        }

        self.fft.process(&mut input, &mut output);

        let half_n = self.n / 2;
        for i in 0..half_n {
            result[i+half_n] = output[i].norm() / (self.n as f32);
        }
        for i in half_n..self.n {
            result[i-half_n] = output[i].norm() / (self.n as f32);
        }

        if self.smoothing_time_constant > 0.0 {
            for i in 0..self.n {
                let x_p = self.prev[i];
                let x_k = result[i];
                result[i] = self.smoothing_time_constant * x_p + (1.0 - self.smoothing_time_constant) * x_k;
            }

            self.prev.copy_from_slice(result);
        }

        for i in 0..self.n {
            // result[i] = log10(result[i]) * 20.0;
            result[i] = result[i].log10() * 10.0;
        }
    }
}
