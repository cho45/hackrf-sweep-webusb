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

- `2 / 4 / 8 / 10 / 20 Msps`

選択ルール:

- `rxSampleRate = min(candidate where candidate >= span)`
- `Span` の最大は `20 MHz`

例:

| Span | 選択される rxSampleRate |
| --- | --- |
| 1.5 MHz | 2 Msps |
| 6 MHz | 8 Msps |
| 10 MHz | 10 Msps |
| 12 MHz | 20 Msps |

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
   - AM: 包絡線 + AGC
   - FM: 位相差分
6. Resampler: `demod_rate -> audioCtx.sampleRate`
7. Audio 出力

FFT 表示パス:

- 入力 IQ（または処理済み IQ）から FFT
- 可視ビンのみ切り出して表示

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

## ライセンス

[LICENSE](../LICENSE) を参照してください。
