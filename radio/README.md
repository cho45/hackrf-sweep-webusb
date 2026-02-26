# Radio SDR (HackRF WebUSB)

HackRF One を WebUSB 経由で制御し、ブラウザ上で

- ウォーターフォール/FFT 表示
- AM / FM 復調
- オーディオ再生

を行う SDR アプリケーションです。

## 現在の入力パラメータ

UI で主に入力するのは次の2つです。

- `Target Frequency`（復調対象周波数）
- `Span`（表示帯域幅）

`IF offset` と `IF band` は自動設定です。

## サンプルレート設計

`rxSampleRate` は `Span` のみを基準に、次の離散候補から最小値を選びます。

- `2 / 4 / 8 / 10 / 12 / 16 / 20 Msps`

選択ルール:

- `rxSampleRate = min(candidate where candidate >= span)`
- `Span` の最大は `20 MHz`

例:

| Span | 選択される rxSampleRate |
| --- | --- |
| 1.5 MHz | 2 Msps |
| 6 MHz | 8 Msps |
| 10 MHz | 10 Msps |
| 12 MHz | 12 Msps |
| 15 MHz | 16 Msps |

## IF offset と中心周波数

- IF offset は固定 `+250 kHz`
- `RF Center = Target + 250 kHz`（下限 1 MHz）
- 目的は DC 近傍の影響回避

表示中心は `Target`、実チューニング中心は `RF Center` です。

## DSP パイプライン（WASM）

現在の復調パスは 2 段デシメーションです。

1. 入力 IQ (`i8`, `rxSampleRate`)
2. DC cancel + NCO ミキシング
3. 粗段デシメーション（boxcar）: `rxSampleRate -> 1 Msps`
4. 復調段デシメーション（FIR band-pass）
   - AM: `1 MHz -> 50 kHz` (`/20`)
   - FM: `1 MHz -> 200 kHz` (`/5`)
5. 復調
   - AM: 包絡線（絶対値）+ DC 除去 + AGC
   - FM: 遅延検波（位相差分）による周波数復調 + ステレオデコード（PLLによる19kHz同期とL-R分離）
6. Resampler: `demod_rate -> audioCtx.sampleRate`
   - ここで復調メソッドに応じたオーディオ帯域のローパスフィルタ（AM: 5 kHz, FM: 15 kHz）も同時に適用し、ノイズをカットします。
7. Audio 出力

FFT 表示パス:

- 入力 IQ（または処理済み IQ）から FFT
- 可視ビンのみ切り出して表示

## スレッドアーキテクチャ

UIのメインスレッドをブロックしないため、処理は以下の3つのスレッドに分散しています。

1. **メインスレッド (UI)**: 描画 (Canvas)、ユーザー入力、WebUSBデバイスの要求
2. **Worker (USB + DSP)**:
   - WebUSB の一括転送 (`transferIn`) ループによる IQ サンプルの継続的な取得
   - WASM (Rust) を呼び出しての DSP パイプライン処理 (NCO, デシメーション, 復調)
   - FFT の計算とメインスレッドへの結果転送
3. **AudioWorklet**:
   - `AudioContext` のレンダーコールバック内でブラウザのオーディオハードウェアへ直接サンプルを供給
   - Worker からの復調済みオーディオサンプルの受信とキューイング

### スレッド間通信

- **Worker ↔ AudioWorklet**: Worker 起動時にメインスレッドを介して `MessageChannel` を作成し、片方の `MessagePort` を AudioWorklet に渡します。これにより、Worker は復調したオーディオバッファ（`Float32Array`）をメインスレッドを経由せずに **AudioWorklet へ直接 `postMessage` (Transferable Objects)** し、ジッターとオーバーヘッドを最小化しています。
- **Worker → メインスレッド**: FFT の結果（スペクトラム描画用）や統計情報を定期的に送信します。

## JS/WASM 境界（低レベル I/O API）

ホットパスの割り当てを減らすため、`Receiver` は低レベル API を持ちます。

- `alloc_io_buffers(maxIqBytes, maxAudioSamples, maxFftBins)`
- `iq_input_ptr() / audio_output_ptr() / fft_output_ptr()`
- `process_iq_len(iqLen) -> audioLen`
- `free_io_buffers()`

Worker は起動時に一度だけ I/O バッファを確保し、各ブロックで

1. IQ を入力バッファへコピー
2. `process_iq_len()` 呼び出し
3. 出力バッファから Audio/FFT を読む

という流れで処理します。

### エラーハンドリング方針

- Rust 側: 受け入れ不可サイズは即エラー（暗黙切り詰めしない）
- JS 側: そのブロックをスキップし、`dropped IQ blocks` 統計へ加算して継続

## パフォーマンス表示（UI）

`DSP`:

- `blocks/s`
- `process ms(avg/max)`
- `cb ms(avg/max)`
- `IQ MB/s`
- `dropped IQ blocks`

`Draw`:

- `fps`
- `draw ms(avg/max)`

`Audio`:

- `buffer`
- `queue / underrun`
- `dropped samples`

## Rust 側のコード構成と読み方

DSPのコア処理は `hackrf-dsp/src` 以下の Rust コードで実装されており、WASM にコンパイルされて JS から呼び出されます。

コードを読む際は、**`lib.rs` を起点** とするのがおすすめです。

- **`lib.rs`**: JS から呼び出される `Receiver` クラス（WASMの入り口）が定義されています。USB から受け取った生データ (`process_iq_len`) を受け取り、NCO ミキシング、デシメーション、復調、FFT の一連のパイプラインを組み立てて実行する「指揮者」の役割を担っています。メモリバッファの管理や SIMD 命令のディスパッチもここで行われます。
- **`filter.rs`**: `BoxcarDecimator`（粗段用）や `FirDecimator`（復調段用）などのデシメーションフィルタが実装されています。FIRフィルタの係数計算ロジックも含まれます。
- **`demod/`**: 復調器の実装ディレクトリです。
  - `am.rs`: AM 復調（絶対値 + 直流カット + AGC）
  - `fm.rs`: FM 復調（遅延検波）およびディエンファシス
  - `fm_stereo.rs`: FM ステレオ分離（19kHz PLL による 38kHz キャリア再生と L-R 復調）
- **`resample.rs`**: 最終段のオーディオリサンプラです。ポリフェーズ FIR フィルタを用いており、オーディオ帯域のローパスフィルタリングも同時に行います。
- **`fft.rs`**: ウォーターフォール描画用のスペクトラムを計算するモジュールです。Rust の `rustfft` クレートをラップし、平滑化（スムージング）などの表示用処理を行っています。

各モジュールには厳密なユニットテストが含まれており、信号処理の数学的妥当性をテストコード自体が説明するドキュメントとして機能しています。

## 開発

前提:

- Node.js（v22 系を想定）
- Rust / cargo
- wasm-pack

### 依存インストール

```sh
cd radio
npm install
```

### WASM ビルド

```sh
cd radio
npm run build:wasm
```

`build:wasm` は通常版（`hackrf-dsp/pkg`）と SIMD 版（`hackrf-dsp/pkg-simd`）の両方を生成します。  
実行時は Worker 側で `simd128` 対応を判定し、対応環境では SIMD 版を、非対応または初期化失敗時は通常版を使います。

### フロント開発サーバ

```sh
cd radio
npm run dev
```

### テスト

```sh
cd radio
npm test
cd hackrf-dsp && cargo test
```

### 本番ビルド

```sh
cd radio
npm run build
```

### DSP ベンチ（ネイティブ）

```sh
cd radio/hackrf-dsp
cargo run --release --bin bench_pipeline
```

必要なケースだけ実行する場合:

```sh
BENCH_MODE=FM BENCH_SR=20 cargo run --release --bin bench_pipeline
```

- `BENCH_MODE`: `AM` / `FM`
- `BENCH_SR`: `20`（Msps）または `20000000`（Hz）

## ライセンス

[LICENSE](../LICENSE) を参照してください。
