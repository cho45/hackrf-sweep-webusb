# Radio SDR (HackRF WebUSB)

HackRF を WebUSB 経由で制御し、ブラウザ上で復調とスペクトログラム（ウォーターフォール）表示を行う SDR (Software Defined Radio) アプリケーションです。

## アーキテクチャ

* **Frontend**: Vite + Vue 3 + TypeScript
  * UI、HackRF の WebUSB 制御、`AudioContext` と `AudioWorkletNode` を用いた音声再生、Canvas (`WaterfallGL`) を用いたスペクトログラム表示。
  * `worker.ts`: WebWorker と Comlink を用い、HackRF の制御と DSP (Wasm) 処理をメインスレッドから分離。
* **Backend (DSP)**: Rust + WebAssembly (`hackrf-dsp`)
  * `wasm-bindgen` を用いた Wasm モジュール。
  * `rustfft` を用いたスペクトル解析。

## DSP パイプライン

受信 IQ データは**表示パス**と**復調パス**の2系統に分かれて処理される。
表示パスは `rxSampleRate`（viewBandwidth に応じて変動）で決まり、復調パスは
モードごとに固定された `demod_rate` で動作する。

```
HackRF IQ @ rxSampleRate (viewBandwidth で決定, 2〜20 MHz)
    │
    ├── [表示パス]
    │     DC Cancel → FFT → Waterfall / Spectrum
    │     rxSampleRate のまま処理。復調とは独立。
    │
    └── [復調パス]
          DC Cancel
            │
          NCO  (rxSampleRate で複素乗算)
            │  target → DC へシフト
            │
          チャンネルフィルタ + デシメーション  (DecimationFilter)
            │  rxSampleRate → demod_rate
            │  factor = rxSampleRate / demod_rate
            │  FIR タップ数: factor に応じて算出
            │  カットオフ: モード別チャンネル帯域幅
            │
          復調  (AM 包絡線検波 / FM 位相差分)
            │  demod_rate のリアル信号を出力
            │
          Resampler (polyphase FIR)
            │  demod_rate → audioCtx.sampleRate (e.g. 48 kHz)
            │  taps_per_phase を step に応じて動的算出
            │  AM: step≈1.04 → 17 taps, WFM: step≈4.2 → ~85 taps
            │
          Audio 出力
```

### モード別パラメータ

| モード | demod_rate | チャンネル帯域幅 |
|--------|-----------|----------------|
| AM     | 50 kHz    | 0〜4.5 kHz     |
| WFM    | 200 kHz   | 0〜100 kHz     |

### 設計上の要点

1. **表示と復調の分離**: `rxSampleRate` は表示帯域に応じて変動するが、
   復調パスの計算量は `demod_rate × FIR タップ数` で決まり、
   `rxSampleRate` に依存しない。
2. **Resampler の入力レートの統一**: 全モードで Resampler の入力を
   ~50 kHz に揃えることで、Resampler の step が常に ~1.0 となり、
   17 タップの polyphase FIR で十分なアンチエイリアシングが得られる。
3. **FIR タップ数の適正化**: デシメーション比（factor）に応じて
   タップ数を算出する。固定 601 タップではなく、チャンネル帯域と
   遷移帯域幅から理論的に必要なタップ数を決定する。

## 開発環境のセットアップ

Node.js と Rust (cargo) が必要です。

### 1. DSPモジュール (Rust/Wasm) のビルド

Wasmモジュールのビルドには `wasm-pack` および `cargo-make` を使用しています。

```sh
cd radio/hackrf-dsp
cargo make build
```

コンパイルが成功すると、`radio/hackrf-dsp/pkg/` に Wasm モジュールと JS バインディングが生成されます。

### 2. DSPモジュールのテスト実行

```sh
cd radio/hackrf-dsp
cargo make test
```

### 3. フロントエンドの依存関係インストールと起動

```sh
cd radio
npm install
npm run dev
```

起動後、ブラウザで指定されたローカルURL（通常 `http://localhost:5173/`）にアクセスしてください。

## 配布用ビルド

全体の TypeScript コンパイルエラーチェックと、Vite による本番用ビルドを行います。

```sh
cd radio
npm run build
```

成功すると `dist/` ディレクトリに配布用の静的ファイルが生成されます。

## 使用方法

1. HackRF を USB で PC に接続します。
2. アプリケーション画面の `Connect` ボタンをクリックします。
3. ブラウザの USBデバイス選択ダイアログで `HackRF` を選択します。
4. `Start Rx` をクリックすると受信が開始され、設定した Target Frequency の復調音声が再生され、ウォーターフォールが表示されます。
5. Frequency や Gain はUI上から動的に変更可能です。

## ライセンス

[LICENSE](../LICENSE) を参照してください。
