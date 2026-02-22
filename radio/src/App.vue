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
          <label>View Center Frequency (kHz)</label>
          <div class="field-input">
            <input type="number" min="1000" step="1" v-model.number="viewCenterFreqKHz" @change="onViewRangeChange" />
          </div>
        </div>

        <div class="field">
          <label>View Bandwidth (kHz)</label>
          <div class="field-input">
            <input type="number" min="100" step="10" v-model.number="viewBandwidthKHz" @change="onViewRangeChange" />
          </div>
          <div class="caption">
            Rx SampleRate: {{ (rxSampleRate / 1_000_000).toFixed(2) }} Msps / Visible: {{ (viewBandwidthHz / 1_000_000).toFixed(3) }} MHz<br>
            RF Center: {{ formatFreq(rfCenterFreq) }} / Target-IF Offset: {{ (ncoOffset / 1000).toFixed(1) }} kHz
          </div>
        </div>

        <div class="field">
          <label>Target Frequency (kHz)</label>
          <div class="field-input">
            <input type="number" min="1000" step="1" v-model.number="targetFreqKHz" @change="onTargetFreqChange" />
          </div>
          <div class="caption">NCO Offset: {{ (ncoOffset / 1000).toFixed(1) }} kHz</div>
        </div>

        <div class="field">
          <label>IF Min (Hz)</label>
          <div class="field-input">
            <input type="number" min="0" step="100" v-model.number="ifMinHz" @change="onIfBandChange" />
          </div>
        </div>

        <div class="field">
          <label>IF Max (Hz)</label>
          <div class="field-input">
            <input type="number" min="1" step="100" v-model.number="ifMaxHz" @change="onIfBandChange" />
          </div>
        </div>
        <div class="caption">AM包絡線検波では通常 IF Min = 0 Hz</div>

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
        <label class="checkbox">
          <input type="checkbox" v-model="dcCancelEnabled"> IQ DC Cancel
        </label>
        <label class="checkbox">
          <input type="checkbox" v-model="fftUseProcessed"> FFT Source: Processed IQ
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
        <div class="axis" style="left: 0% ">{{ formatFreq(displayMinFreq) }}</div>
        <div class="axis" style="left: 25% ">{{ formatFreq(displayMinFreq + viewBandwidthHz * 0.25) }}</div>
        <div class="axis" style="left: 50% ">{{ formatFreq(viewCenterFreq) }}</div>
        <div class="axis" style="left: 75%">{{ formatFreq(displayMinFreq + viewBandwidthHz * 0.75) }}</div>
        <div class="axis right" style="right: 0%">{{ formatFreq(displayMaxFreq) }}</div>
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
const decimationFactor = 40;  // 基本の復調系ダウンサンプリング比
const minTuneFreqHz = 1_000_000;
const minDisplayBandwidthHz = 100_000;
const maxHackRFSampleRate = 20_000_000;
const minHackRFSampleRate = 2_000_000;
const sampleRateStepHz = 100_000;
const usableBandwidthRatio = 0.75; // HackRFのBBフィルタ想定帯域
const forcedTargetOffsetHz = 250_000; // target と RF center の最小分離（DC回避）

const viewCenterFreq = ref(1_025_000);
const viewBandwidthHz = ref(1_500_000);
const targetFreq = ref(1_025_000);
const rfCenterFreq = ref(1_275_000);
const rxSampleRate = ref(2_000_000);
const ifMinHz = ref(0);
const ifMaxHz = ref(4_500);
const dcCancelEnabled = ref(true);
const fftUseProcessed = ref(true);

const maxDisplayBandwidthHz =
  maxHackRFSampleRate * usableBandwidthRatio - 2 * forcedTargetOffsetHz;
const displayMinFreq = computed(() => viewCenterFreq.value - viewBandwidthHz.value / 2);
const displayMaxFreq = computed(() => viewCenterFreq.value + viewBandwidthHz.value / 2);
const ncoOffset = computed(() => targetFreq.value - rfCenterFreq.value);

const targetFreqKHz = computed({
  get: () => targetFreq.value / 1000,
  set: (val) => { targetFreq.value = val * 1000; }
});

const viewCenterFreqKHz = computed({
  get: () => viewCenterFreq.value / 1000,
  set: (val) => { viewCenterFreq.value = val * 1000; }
});

const viewBandwidthKHz = computed({
  get: () => viewBandwidthHz.value / 1000,
  set: (val) => { viewBandwidthHz.value = val * 1000; }
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

const chooseSampleRate = (requiredUsableBandwidth: number) => {
  const required = requiredUsableBandwidth / usableBandwidthRatio;
  const stepped = Math.ceil(required / sampleRateStepHz) * sampleRateStepHz;
  return Math.max(minHackRFSampleRate, Math.min(maxHackRFSampleRate, stepped));
};

const clampTargetIntoView = () => {
  const minHz = Math.max(minTuneFreqHz, displayMinFreq.value);
  const maxHz = Math.max(minHz, displayMaxFreq.value);
  if (targetFreq.value < minHz) targetFreq.value = minHz;
  if (targetFreq.value > maxHz) targetFreq.value = maxHz;
};

const chooseRfCenterForTarget = () => {
  const candidates = [
    targetFreq.value - forcedTargetOffsetHz,
    targetFreq.value + forcedTargetOffsetHz,
  ].filter((hz) => hz >= minTuneFreqHz);

  if (candidates.length === 0) {
    return minTuneFreqHz;
  }

  return candidates.reduce((best, cur) =>
    Math.abs(cur - viewCenterFreq.value) < Math.abs(best - viewCenterFreq.value) ? cur : best
  );
};

const normalizeViewRange = () => {
  if (viewCenterFreq.value < minTuneFreqHz) viewCenterFreq.value = minTuneFreqHz;
  if (viewBandwidthHz.value < minDisplayBandwidthHz) viewBandwidthHz.value = minDisplayBandwidthHz;
  if (viewBandwidthHz.value > maxDisplayBandwidthHz) viewBandwidthHz.value = maxDisplayBandwidthHz;
  clampTargetIntoView();
  rfCenterFreq.value = chooseRfCenterForTarget();

  const requiredUsable =
    viewBandwidthHz.value + 2 * Math.abs(viewCenterFreq.value - rfCenterFreq.value);
  rxSampleRate.value = chooseSampleRate(requiredUsable);
};

const restartRx = async () => {
  if (!running.value) return;
  await stop();
  await start();
};

const onViewRangeChange = async () => {
  normalizeViewRange();
  await restartRx();
};

const onTargetFreqChange = async () => {
  normalizeViewRange();
  await restartRx();
};

normalizeViewRange();

const computeFftViewWindow = (fftSize: number) => {
  const sampleRate = rxSampleRate.value;
  const toBin = (relHz: number) => (relHz / sampleRate + 0.5) * fftSize;
  const minRel = displayMinFreq.value - rfCenterFreq.value;
  const maxRel = displayMaxFreq.value - rfCenterFreq.value;
  const desiredBins = Math.max(1, Math.ceil(toBin(maxRel) - toBin(minRel)));
  let startBin = Math.floor(toBin(minRel));
  let endBin = startBin + desiredBins;

  if (startBin < 0) {
    endBin -= startBin;
    startBin = 0;
  }
  if (endBin > fftSize) {
    startBin -= endBin - fftSize;
    endBin = fftSize;
  }
  if (startBin < 0) {
    startBin = 0;
  }
  if (endBin <= startBin) {
    endBin = Math.min(fftSize, startBin + 1);
  }

  return {
    startBin,
    bins: endBin - startBin,
  };
};

const onIfBandChange = async () => {
  if (ifMinHz.value < 0) ifMinHz.value = 0;
  if (ifMaxHz.value <= ifMinHz.value) {
    ifMaxHz.value = ifMinHz.value + 100;
  }

  if (backend && running.value) {
    await backend.setIfBand(ifMinHz.value, ifMaxHz.value);
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

  normalizeViewRange();
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
  let fftSizeFull = Math.pow(2, Math.ceil(Math.log2(freqBinCount0)));
  if (fftSizeFull < 256) fftSizeFull = 256;
  if (fftSizeFull > 8192) fftSizeFull = 8192;

  const fftViewWindow = computeFftViewWindow(fftSizeFull);
  const fftVisibleBins = fftViewWindow.bins;

  // キャンバスの内部解像度を FFT の bin 数に合わせる
  canvasFft.width = fftVisibleBins;
  canvasFft.height = 200;

  const maxTextureSize = 16384; // Typical max texture size for WebGL
  const useWebGL = fftVisibleBins <= maxTextureSize;

  waterfall = useWebGL ?
    new WaterfallGL(canvasWf, fftVisibleBins, 256) :
    new Waterfall(canvasWf, fftVisibleBins, 256);

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
      for (let i = 0; i < fftOut.length; i++) {
        const val = fftOut[i] !== undefined ? fftOut[i]! : -120;
        const n = (val + 45) / 42; // Adjust for visualization range
        canvasFftCtx.lineTo(i, canvasFft.height - canvasFft.height * n);
      }
      canvasFftCtx.strokeStyle = "#fff";
      canvasFftCtx.stroke();

      // targetFreq (NCOオフセット位置) に赤い線を引く
      const targetHz = targetFreq.value;
      const startHz = displayMinFreq.value;
      const widthHz = viewBandwidthHz.value;
      
      const ratio = Math.min(1, Math.max(0, (targetHz - startHz) / widthHz));
      const x = canvasFft.width * ratio;

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
    sampleRate: rxSampleRate.value,
    centerFreq: rfCenterFreq.value,
    targetFreq: targetFreq.value,
    decimationFactor,
    outputSampleRate: audioCtx!.sampleRate, // Use actual audio context sample rate
    fftSize: fftSizeFull,
    fftVisibleStartBin: fftViewWindow.startBin,
    fftVisibleBins,
    ifMinHz: ifMinHz.value,
    ifMaxHz: ifMaxHz.value,
    dcCancelEnabled: dcCancelEnabled.value,
    fftUseProcessed: fftUseProcessed.value,
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
watch(() => dcCancelEnabled.value, (val) => {
  if (connected.value && running.value) backend.setDcCancelEnabled(val);
});
watch(() => fftUseProcessed.value, (val) => {
  if (connected.value && running.value) backend.setFftUseProcessed(val);
});

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
