<template>
  <div id="app">
    <div class="actions">
      <div style="margin-bottom: 20px;">
        <template v-if="!connected">
          <button class="btn btn-primary" v-on:click="connect">Connect</button>
        </template>
        <template v-else>
          <button class="btn btn-primary" v-on:click="start" v-if="!running">Start Rx</button>
          <button class="btn btn-secondary" v-on:click="stop" v-if="running">Stop Rx</button>
          <button class="btn" v-on:click="disconnect">Disconnect</button>
        </template>
      </div>

      <div class="form">
        <div class="field">
          <label>Target Frequency (kHz)</label>
          <div class="field-input">
            <input type="number" v-model.number="targetFreqKHz" @change="onFreqChange" />
          </div>
          <div class="caption">NCO Offset: {{ (ncoOffset / 1000).toFixed(1) }} kHz</div>
        </div>

        <div class="divider"></div>

        <div class="field">
          <label>LNA Gain (IF)</label>
          <div class="field-input">
            <input type="range" min="0" max="40" step="8" v-model.number="options.lnaGain" class="field-range" />
            <input type="number" min="0" max="40" step="8" v-model.number="options.lnaGain" class="field-number" />
            <span class="field-suffix">dB</span>
          </div>
        </div>

        <div class="field">
          <label>VGA Gain (Baseband)</label>
          <div class="field-input">
            <input type="range" min="0" max="62" step="2" v-model.number="options.vgaGain" class="field-range" />
            <input type="number" min="0" max="62" step="2" v-model.number="options.vgaGain" class="field-number" />
            <span class="field-suffix">dB</span>
          </div>
        </div>
        <div class="divider"></div>

        <label class="checkbox">
          <input type="checkbox" v-model="options.ampEnabled"> RF Amp (14dB)
        </label>
        <label class="checkbox">
          <input type="checkbox" v-model="options.antennaEnabled"> Antenna Port Power
        </label>
      </div>

      <div class="body-2" style="margin-top: 20px;" v-if="connected">
        {{ info.boardName }}<br>
        {{ info.firmwareVersion }}
      </div>

      <div class="snackbar" :class="{ show: snackbar.show }">
        {{ snackbar.message }}
      </div>
    </div>
    
    <div class="canvas-container" ref="canvasContainer">
      <div style="width: 100%; height: 70vh; position: relative">
        <canvas id="waterfall" ref="waterfallCanvas"></canvas>
      </div>
      <div style="width: 100%; height: 30vh; position: relative">
        <canvas id="fft" ref="fftCanvas"></canvas>
        <div class="axis" style="left: 0% ">{{ formatFreq(centerFreq - sampleRate/2) }}</div>
        <div class="axis" style="left: 25% ">{{ formatFreq(centerFreq - sampleRate/4) }}</div>
        <div class="axis" style="left: 50% ">{{ formatFreq(centerFreq) }}</div>
        <div class="axis" style="left: 75%">{{ formatFreq(centerFreq + sampleRate/4) }}</div>
        <div class="axis right" style="right: 0%">{{ formatFreq(centerFreq + sampleRate/2) }}</div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, reactive, computed, onUnmounted, watch } from 'vue';
import * as Comlink from 'comlink';
import { WaterfallGL, Waterfall } from './utils';
import { HackRF } from './hackrf';

// comlink 経由でバックエンド(WASM/HackRF処理)をロード
const WorkerBackend = Comlink.wrap<any>(new Worker(new URL('./worker.ts', import.meta.url), { type: 'module' }));

const connected = ref(false);
const running = ref(false);
const snackbar = reactive({ show: false, message: '' });

// HackRF Info
const info = reactive({ boardName: '', firmwareVersion: '' });

// 受信パラメータ
const sampleRate = 2_000_000; // 2MHz
const decimationFactor = 40;  // 2MHz / 40 = 50kHz オーディオレート
const targetFreq = ref(1_025_000); // デフォルト 1025kHz に設定

// NCOのオフセットを計算 (下限制御のために centerFreq を sampleRate/2 以上に保つ)
const minCenterFreq = sampleRate / 2; // 1,000,000 Hz 
const centerFreq = ref(Math.max(targetFreq.value - 250_000, minCenterFreq)); 
const ncoOffset = ref(targetFreq.value - centerFreq.value);

const targetFreqKHz = computed({
  get: () => targetFreq.value / 1000,
  set: (val) => { targetFreq.value = val * 1000; }
});

const options = reactive({
  ampEnabled: false,
  antennaEnabled: false,
  lnaGain: 16,
  vgaGain: 16,
});

let backend: any = null;
let audioCtx: AudioContext | null = null;
let audioNode: AudioWorkletNode | null = null;
let audioModuleLoaded = false;


const waterfallCanvas = ref<HTMLCanvasElement | null>(null);
const fftCanvas = ref<HTMLCanvasElement | null>(null);

let waterfall: WaterfallGL | Waterfall | null = null;

const showSnackbar = (msg: string) => {
  snackbar.message = msg;
  snackbar.show = true;
  setTimeout(() => { snackbar.show = false; }, 3000);
};

// 桁合わせ用のヘルパー
const formatFreq = (hz: number) => {
  return (hz / 1_000_000).toFixed(3) + " MHz";
};

const onFreqChange = async () => {
  // アプリケーション側で中心周波数の下限(1MHz)を保障し、負の開始周波数の発生を防ぐ
  const minCenterFreq = sampleRate / 2;
  centerFreq.value = Math.max(targetFreq.value - 250_000, minCenterFreq);
  ncoOffset.value = targetFreq.value - centerFreq.value;

  if (backend && running.value) {
    // ソフトウェア(WASM側のNCO)のオフセット追従と
    // ハードウェア(HackRFのLO)の再チューニングを同時に実行する
    await backend.setFreq(centerFreq.value);
    await backend.setTargetFreq(centerFreq.value, targetFreq.value);
  }
};

const connect = async () => {
  if (!backend) {
    backend = await new (WorkerBackend as any)();
    await backend.init();
  }

  try {
    let ok = await backend.open();
    if (!ok) {
        const device = await HackRF.requestDevice();
        if (!device) {
            showSnackbar("device is not found");
            return;
        }
        ok = await backend.open({
            vendorId: device.vendorId,
            productId: device.productId,
            serialNumber: device.serialNumber
        });
        if (!ok) {
            showSnackbar("failed to open device in backend");
            return;
        }
    }
    connected.value = true;
    const deviceInfo = await backend.info();
    info.boardName = "HackRF (ID: " + deviceInfo.boardId + ")";
    info.firmwareVersion = deviceInfo.versionString;

    // Gain setup
    await backend.setVgaGain(options.vgaGain);
    await backend.setLnaGain(options.lnaGain);
    await backend.setAmpEnable(options.ampEnabled);
    await backend.setAntennaEnable(options.antennaEnabled);

    showSnackbar("Connected successfully");
  } catch (e: any) {
    showSnackbar("Connection Error: " + e.message);
  }
};

const disconnect = async () => {
  if (backend) {
    if (running.value) await stop();
    await backend.close();
  }
  connected.value = false;
  showSnackbar("Disconnected");
};

const initAudio = async () => {
  if (!audioCtx) {
    audioCtx = new AudioContext();
  }

  if (!audioModuleLoaded) {
    await audioCtx.audioWorklet.addModule('/audio-stream-processor.js');
    audioModuleLoaded = true;
  }

  if (!audioNode) {
    audioNode = new AudioWorkletNode(audioCtx, 'audio-stream-processor', {
      numberOfInputs: 0,
      numberOfOutputs: 1,
      outputChannelCount: [1],
    });
    audioNode.connect(audioCtx.destination);
  }

  await audioCtx.resume();
};

const playAudioBuffer = (data: Float32Array) => {
  if (!audioNode) return;
  // WASMメモリ再利用の影響を避けるため、明示的にコピーしてからWorkletへ渡す。
  const chunk = new Float32Array(data);
  audioNode.port.postMessage({ type: 'push', data: chunk }, [chunk.buffer]);
};

const stopAudio = () => {
  if (audioNode) {
    audioNode.port.postMessage({ type: 'reset' });
  }
  if (audioCtx) {
    void audioCtx.suspend();
  }
};

const start = async () => {
  if (!connected.value) return;

  await initAudio();

  if (!fftCanvas.value || !waterfallCanvas.value) {
    console.error("Canvas elements not found.");
    return;
  }

  const canvasFft = fftCanvas.value;
  const canvasWf = waterfallCanvas.value;
  const canvasFftCtx = canvasFft.getContext('2d');

  if (!canvasFftCtx) {
    console.error("FFT Canvas 2D context not available.");
    return;
  }

  // FFTサイズはキャンバスの幅を元に、次数の大きい直近の「2のべき乗」に合わせる
  const freqBinCount0 = canvasFft.offsetWidth * window.devicePixelRatio;
  let fftSize = Math.pow(2, Math.ceil(Math.log2(freqBinCount0)));
  if (fftSize < 256) fftSize = 256;
  if (fftSize > 8192) fftSize = 8192; // 上限

  // キャンバスの内部解像度を FFT の bin 数に合わせる
  canvasFft.width = fftSize;
  canvasFft.height = 200;

  const maxTextureSize = 16384; // Typical max texture size for WebGL
  const useWebGL = fftSize <= maxTextureSize;

  waterfall = useWebGL ?
    new WaterfallGL(canvasWf, fftSize, 256) :
    new Waterfall(canvasWf, fftSize, 256);

  // Comlinkのコールバック関数は proxy に包む必要がある
  const onData = Comlink.proxy((audioOut: Float32Array, fftOut: Float32Array) => {
    playAudioBuffer(audioOut);
    if (waterfall) {
      waterfall.renderLine(fftOut);

      // FFT表示
      canvasFftCtx.clearRect(0, 0, canvasFft.width, canvasFft.height);
      canvasFftCtx.save();
      canvasFftCtx.beginPath();
      canvasFftCtx.moveTo(0, canvasFft.height);
      for (let i = 0; i < fftSize; i++) {
        // fftOut は長さ n の Float32Array であり、i < fftSize の範囲内なので安全
        const val = fftOut[i] !== undefined ? fftOut[i]! : -100;
        const n = (val + 45) / 42; // Adjust for visualization range
        canvasFftCtx.lineTo(i, canvasFft.height - canvasFft.height * n);
      }
      canvasFftCtx.strokeStyle = "#fff";
      canvasFftCtx.stroke();

      // targetFreq (NCOオフセット位置) に赤い線を引く
      // freqBinCount と同じく、キャンバスの幅 = サンプリングレート(2MHz) の帯域幅
      // targetFreq は centerFreq から targetFreqKHz の差分に相当するため、開始位置は centerFreq - sampleRate/2
      const targetHz = targetFreq.value;
      const startHz = centerFreq.value - sampleRate / 2;
      
      // 全体 (sampleRate) のうち、現在の targetHz は startHz からどれだけ進んだか
      const ratio = (targetHz - startHz) / sampleRate;
      const x = fftSize * ratio;

      canvasFftCtx.beginPath();
      canvasFftCtx.moveTo(x, 0);
      canvasFftCtx.lineTo(x, canvasFft.height);
      canvasFftCtx.strokeStyle = "rgba(255, 0, 0, 0.8)";
      canvasFftCtx.lineWidth = 1;
      canvasFftCtx.stroke();

      canvasFftCtx.restore();
    }
  });

  await backend.startRx({
    sampleRate,
    centerFreq: centerFreq.value,
    targetFreq: targetFreq.value,
    decimationFactor,
    outputSampleRate: audioCtx!.sampleRate, // Use actual audio context sample rate
    fftSize,
  }, onData);

  running.value = true;
};

const stop = async () => {
  if (backend) {
    await backend.stopRx();
  }
  stopAudio();
  running.value = false;
};

// オプションの監視
watch(() => options.lnaGain, (val) => { if (connected.value) backend.setLnaGain(val); });
watch(() => options.vgaGain, (val) => { if (connected.value) backend.setVgaGain(val); });
watch(() => options.ampEnabled, (val) => { if (connected.value) backend.setAmpEnable(val); });
watch(() => options.antennaEnabled, (val) => { if (connected.value) backend.setAntennaEnable(val); });

onUnmounted(() => {
  disconnect();
});
</script>

<style>
/* Base */
html,
body, #app {
	margin: 0;
	padding: 0;
	font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Oxygen, Ubuntu, Cantarell, sans-serif;
	font-size: 14px;
	height: 100vh;
	width: 100vw;
	overflow: hidden;
	background: #000;
}

#app {
	display: flex;
}

/* Layout */
.actions {
	padding: 16px;
	width: 280px;
	flex-shrink: 0;
	background: #111;
	color: #ccc;
	border-right: 1px solid #333;
	box-sizing: border-box;
	overflow-y: auto;
}
.actions h2 {
	margin-top: 0;
	color: #fff;
	font-weight: 500;
}

.canvas-container {
	flex-grow: 1;
	background: #000;
	display: flex;
	flex-direction: column;
}

#fft,
#waterfall {
	width: 100%;
	height: 100%;
	display: block;
}

/* Buttons */
.btn {
	display: inline-block;
	padding: 8px 16px;
	margin: 4px;
	border: none;
	border-radius: 4px;
	background: #4caf50;
	color: #fff;
	font-size: 14px;
	text-transform: uppercase;
	cursor: pointer;
	transition: background 0.2s;
}

.btn:hover:not(:disabled) {
	background: #43a047;
}

.btn:disabled {
	opacity: 0.5;
	cursor: not-allowed;
}

.btn-primary {
	background: #2196f3;
}

.btn-primary:hover:not(:disabled) {
	background: #1e88e5;
}

.btn-secondary {
	background: #757575;
}

.btn-secondary:hover:not(:disabled) {
	background: #616161;
}

/* Form */
.form {
	margin-top: 16px;
}

.field {
	margin-bottom: 12px;
}

.field label {
	display: block;
	margin-bottom: 4px;
	font-size: 12px;
	color: #888;
}

.field-input {
	position: relative;
	display: flex;
	align-items: center;
}

.field-input input {
	flex: 1;
	padding: 8px 12px;
	border: 1px solid #444;
	border-radius: 4px;
	background: #222;
	color: #fff;
	font-size: 14px;
	min-width: 0;
}

.field-input input:focus {
	outline: none;
	border-color: #2196f3;
}

.field-input .field-range {
	flex: 2;
	margin-right: 8px;
	padding: 0;
}

.field-input .field-number {
	flex: 1;
}

.field-suffix {
	position: absolute;
	right: 12px;
	color: #888;
	font-size: 12px;
	pointer-events: none;
}

.checkbox {
	display: flex;
	align-items: center;
	margin-bottom: 8px;
	cursor: pointer;
	color: #ccc;
}
.checkbox input {
	margin-right: 8px;
}

.divider {
	height: 1px;
	background: #333;
	margin: 16px 0;
}

.caption {
	margin: 8px 0;
	font-size: 12px;
	color: #888;
}
.body-2 {
	font-size: 12px;
	color: #aaa;
}

/* Snackbar */
.snackbar {
	position: fixed;
	bottom: 16px;
	left: 50%;
	transform: translateX(-50%);
	padding: 12px 24px;
	background: #323232;
	color: #fff;
	border-radius: 4px;
	font-size: 14px;
	z-index: 1000;
	opacity: 0;
	pointer-events: none;
	transition: opacity 0.3s;
}

.snackbar.show {
	opacity: 1;
}

/* Axis labels */
.axis {
	position: absolute;
	top: 0;
	font-weight: bold;
	font-size: 14px;
	color: #fff;
	border-left: 2px solid #f33;
	padding: 0 4px;
	background: rgba(0, 0, 0, 0.5);
	white-space: nowrap;
	pointer-events: none;
}

.axis.right {
	border-left: none;
	border-right: 2px solid #f33;
	transform: translateX(-100%);
}
</style>
