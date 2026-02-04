/* @ts-self-types="./hackrf_web.d.ts" */

import * as wasm from "./hackrf_web_bg.wasm";
import { __wbg_set_wasm } from "./hackrf_web_bg.js";
__wbg_set_wasm(wasm);
wasm.__wbindgen_start();
export {
    FFT, set_panic_hook
} from "./hackrf_web_bg.js";
export { wasm as __wasm }
