/* tslint:disable */
/* eslint-disable */

export class FFT {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * FFTを実行し、結果をdBスケールで出力する。
     *
     * # 入力形式
     * * `input_` - i8の配列として表現された複素数列 `[re0, im0, re1, im1, ...]`
     *               長さは `self.n * 2` でなければならない
     *
     * # 出力形式
     * * `result` - 結果を格納するバッファ。長さは `self.n` でなければならない
     *   - `result[0 .. half_n]` - 負の周波数成分（DC中心配置、dBスケール）
     *   - `result[half_n .. n]` - 正の周波数成分（DC中心配置、dBスケール）
     *
     * # コントラクト（呼び出し側の責任）
     * * `input_.len() == self.n * 2` でなければならない
     * * `result.len() == self.n` でなければならない
     *
     * # 安全性
     * この関数は unsafe なメモリ再解釈を使用する。コントラクトに違反する場合、
     * 未定義動作を引き起こす可能性がある。
     */
    fft(input_: Int8Array, result: Float32Array): void;
    /**
     * 新しいFFTプロセッサを作成する。
     *
     * # 引数
     * * `n` - FFTサイズ。2の累乗であり、0より大きい必要がある
     * * `window_` - 窓関数の配列。長さは `n` と等しくなければならない
     *
     * # パニック
     * * `n` が 0 の場合
     * * `n` が 2の累乗でない場合
     * * `window_.len() != n` の場合
     */
    constructor(n: number, window_: Float32Array);
    set_smoothing_time_constant(val: number): void;
}

export function set_panic_hook(): void;
