# Radio SDR (HackRF WebUSB)

HackRF を WebUSB 経由で制御し、ブラウザ上で AM 復調とスペクトログラム（ウォーターフォール）表示を行う SDR (Software Defined Radio) アプリケーションです。

## アーキテクチャ

* **Frontend**: Vite + Vue 3 + TypeScript
  * UI、HackRF の WebUSB 制御、`AudioContext` と `AudioBufferSourceNode` を用いた音声再生、Canvas (`WaterfallGL`) を用いたスペクトログラム表示。
  * `worker.ts`: WebWorker と Comlink を用い、HackRF の制御と DSP (Wasm) 処理をメインスレッドから分離。
* **Backend (DSP)**: Rust + WebAssembly (`hackrf-dsp`)
  * `wasm-bindgen` を用いた Wasm モジュール。
  * NCO による複素ベースバンド周波数シフト、FIR フィルタによるデシメーション、AM 復調 (包絡線検波)。
  * Polyphase Anti-aliasing Resampler を用いた AudioContext ネイティブレート（例: 48kHz）へのリサンプリング。
  * `rustfft` を用いたスペクトル解析。

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
4. `Start Rx` をクリックすると受信が開始され、設定した Target Frequency の AM 復調音声が再生され、ウォーターフォールが表示されます。
5. Frequency や Gain はUI上から動的に変更可能です。

## ライセンス

[LICENSE](../LICENSE) を参照してください。
