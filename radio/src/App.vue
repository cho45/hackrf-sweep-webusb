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
          <label>Span</label>
          <div class="field-input">
            <button type="button" class="field-input-btn" @click="openKeypad('span')">
              {{ formatInputFreq(spanHz) }}
            </button>
          </div>
          <div class="caption">
            Rx SampleRate: {{ (rxSampleRate / 1_000_000).toFixed(2) }} Msps / Visible: {{ (spanHz / 1_000_000).toFixed(3) }} MHz<br>
            View Center: {{ formatFreq(viewCenterFreq) }} / RF Center: {{ formatFreq(rfCenterFreq) }} / IF Offset: {{ (Math.abs(ncoOffset) / 1000).toFixed(1) }} kHz
          </div>
        </div>

        <div class="field">
          <label>Target Frequency</label>
          <div class="field-input">
            <button type="button" class="field-input-btn" @click="openKeypad('target')">
              {{ formatInputFreq(targetFreq) }}
            </button>
          </div>
          <div class="caption">
            Demod Target: {{ formatFreq(targetFreq) }} / IF Filter: {{ ifMinHz.toLocaleString() }} - {{ ifMaxHz.toLocaleString() }} Hz (Auto)
          </div>
        </div>

        <div class="field">
          <label>Demod Mode</label>
          <div class="field-input">
            <select v-model="demodMode" @change="onDemodModeChange" style="flex:1; padding:8px 12px; border:1px solid #444; border-radius:4px; background:#222; color:#fff; font-size:14px;">
              <option value="AM">AM</option>
              <option value="FM">FM (WFM)</option>
            </select>
          </div>
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
        <div class="axis" style="left: 25% ">{{ formatFreq(displayMinFreq + spanHz * 0.25) }}</div>
        <div class="axis" style="left: 50% ">{{ formatFreq(viewCenterFreq) }}</div>
        <div class="axis" style="left: 75%">{{ formatFreq(displayMinFreq + spanHz * 0.75) }}</div>
        <div class="axis right" style="right: 0%">{{ formatFreq(displayMaxFreq) }}</div>
      </div>
    </div>
    <Keypad
      v-if="keypadField"
      :key="`keypad-${keypadField}-${keypadOpenToken}`"
      :title="keypadTitle"
      :unit="keypadUnit"
      :model-value="keypadInitialValue"
      @submit="onKeypadSubmit"
      @close="closeKeypad"
    />
  </div>
</template>

<script setup lang="ts">
import { ref, reactive, computed, onUnmounted, watch } from 'vue';
import * as Comlink from 'comlink';
import { WaterfallGL, Waterfall } from './utils';
import { HackRF } from './hackrf';
import Keypad from './components/Keypad.vue';

// comlink 経由でバックエンド(WASM/HackRF処理)をロード
const WorkerBackend = Comlink.wrap<any>(new Worker(new URL('./worker.ts', import.meta.url), { type: 'module' }));

const connected = ref(false);
const running = ref(false);
const snackbar = reactive({ show: false, message: '' });

// HackRF Info
const info = reactive({ boardName: '', firmwareVersion: '' });

// 受信パラメータ
const minTuneFreqHz = 1_000_000;
const minDisplayBandwidthHz = 100_000;
const maxHackRFSampleRate = 20_000_000;
const minHackRFSampleRate = 2_000_000;
const sampleRateStepHz = 100_000;
const ifOffsetHz = 250_000; // target からこの分だけRF centerをずらしてDC回避

const settingsStorageKey = 'radio.settings.v2';
const isDemodMode = (mode: unknown): mode is 'AM' | 'FM' => mode === 'AM' || mode === 'FM';
type PersistedSettings = {
  spanHz: number;
  targetFreq: number;
  dcCancelEnabled: boolean;
  fftUseProcessed: boolean;
  ampEnabled: boolean;
  antennaEnabled: boolean;
  lnaGain: number;
  vgaGain: number;
  demodMode: 'AM' | 'FM';
};

const defaultSettings: PersistedSettings = {
  spanHz: 1_500_000,
  targetFreq: 1_025_000,
  dcCancelEnabled: true,
  fftUseProcessed: true,
  ampEnabled: false,
  antennaEnabled: false,
  lnaGain: 16,
  vgaGain: 16,
  demodMode: 'AM',
};

const loadSettings = (): PersistedSettings => {
  try {
    const raw = localStorage.getItem(settingsStorageKey) ?? localStorage.getItem('radio.settings.v1');
    if (!raw) return { ...defaultSettings };
    const parsed = JSON.parse(raw) as Partial<PersistedSettings> & { viewBandwidthHz?: number };

    const getNumber = (key: keyof PersistedSettings) => {
      const value = parsed[key];
      return typeof value === 'number' && Number.isFinite(value)
        ? value
        : defaultSettings[key] as number;
    };
    const getBoolean = (key: keyof PersistedSettings) => {
      const value = parsed[key];
      return typeof value === 'boolean'
        ? value
        : defaultSettings[key] as boolean;
    };

    return {
      spanHz: typeof parsed.spanHz === 'number'
        ? parsed.spanHz
        : (typeof parsed.viewBandwidthHz === 'number' ? parsed.viewBandwidthHz : defaultSettings.spanHz),
      targetFreq: getNumber('targetFreq'),
      dcCancelEnabled: getBoolean('dcCancelEnabled'),
      fftUseProcessed: getBoolean('fftUseProcessed'),
      ampEnabled: getBoolean('ampEnabled'),
      antennaEnabled: getBoolean('antennaEnabled'),
      lnaGain: getNumber('lnaGain'),
      vgaGain: getNumber('vgaGain'),
      demodMode: isDemodMode(parsed.demodMode) ? parsed.demodMode : defaultSettings.demodMode,
    };
  } catch {
    return { ...defaultSettings };
  }
};

const loadedSettings = loadSettings();

const spanHz = ref(loadedSettings.spanHz);
const targetFreq = ref(loadedSettings.targetFreq);
const rxSampleRate = ref(2_000_000);
const dcCancelEnabled = ref(loadedSettings.dcCancelEnabled);
const fftUseProcessed = ref(loadedSettings.fftUseProcessed);
const demodMode = ref(loadedSettings.demodMode);

const defaultIfBandForMode = (mode: string): { minHz: number; maxHz: number } => {
  return mode === 'FM' ? { minHz: 0, maxHz: 75_000 } : { minHz: 0, maxHz: 4_500 };
};

const maxSpanHz = maxHackRFSampleRate - 2 * Math.abs(ifOffsetHz);
const ifBand = computed(() => defaultIfBandForMode(demodMode.value));
const ifMinHz = computed(() => ifBand.value.minHz);
const ifMaxHz = computed(() => ifBand.value.maxHz);
const viewCenterFreq = computed(() => targetFreq.value);
const rfCenterFreq = computed(() => Math.max(minTuneFreqHz, targetFreq.value + ifOffsetHz));
const displayMinFreq = computed(() => viewCenterFreq.value - spanHz.value / 2);
const displayMaxFreq = computed(() => viewCenterFreq.value + spanHz.value / 2);
const ncoOffset = computed(() => targetFreq.value - rfCenterFreq.value);

type KeypadField = 'target' | 'span';
type DisplayUnit = 'Hz' | 'kHz' | 'MHz';
const keypadField = ref<KeypadField | null>(null);
const keypadOpenToken = ref(0);
const keypadTitle = computed(() =>
  keypadField.value === 'span' ? 'Span' : 'Target Frequency'
);
const pickDisplayUnit = (hz: number): DisplayUnit => {
  if (Math.abs(hz) >= 1_000_000) return 'MHz';
  if (Math.abs(hz) >= 1_000) return 'kHz';
  return 'Hz';
};
const unitFactor = (unit: DisplayUnit): number => {
  if (unit === 'MHz') return 1_000_000;
  if (unit === 'kHz') return 1_000;
  return 1;
};
const keypadUnit = computed<DisplayUnit>(() => {
  if (keypadField.value === 'span') return pickDisplayUnit(spanHz.value);
  return pickDisplayUnit(targetFreq.value);
});
const keypadInitialValue = computed(() => {
  return '0';
});

const options = reactive({
  ampEnabled: loadedSettings.ampEnabled,
  antennaEnabled: loadedSettings.antennaEnabled,
  lnaGain: loadedSettings.lnaGain,
  vgaGain: loadedSettings.vgaGain,
});

let backend: any = null;
let audioCtx: AudioContext | null = null;
let audioNode: AudioWorkletNode | null = null;
let audioModuleLoaded = false;


const waterfallCanvas = ref<HTMLCanvasElement | null>(null);
const fftCanvas = ref<HTMLCanvasElement | null>(null);

let waterfall: WaterfallGL | Waterfall | null = null;
let latestFftFrame: Float32Array | null = null;
let renderLoopId: number | null = null;
let renderLastTimeMs = 0;
const waterfallFps = 30;
const waterfallFrameIntervalMs = 1000 / waterfallFps;

const showSnackbar = (msg: string) => {
  snackbar.message = msg;
  snackbar.show = true;
  setTimeout(() => { snackbar.show = false; }, 3000);
};

// 桁合わせ用のヘルパー
const formatFreq = (hz: number) => {
  return (hz / 1_000_000).toFixed(3) + " MHz";
};

const formatInputFreq = (hz: number) => {
  const unit = pickDisplayUnit(hz);
  const factor = unitFactor(unit);
  const value = hz / factor;
  const precision = unit === 'Hz' ? 0 : 3;
  return `${value.toFixed(precision)} ${unit}`;
};

const chooseSampleRate = (requiredBandwidth: number) => {
  const stepped = Math.ceil(requiredBandwidth / sampleRateStepHz) * sampleRateStepHz;
  return Math.max(minHackRFSampleRate, Math.min(maxHackRFSampleRate, stepped));
};

const normalizeTuning = () => {
  if (targetFreq.value < minTuneFreqHz) targetFreq.value = minTuneFreqHz;
  if (spanHz.value < minDisplayBandwidthHz) spanHz.value = minDisplayBandwidthHz;
  if (spanHz.value > maxSpanHz) spanHz.value = maxSpanHz;

  const requiredBandwidth =
    spanHz.value + 2 * Math.abs(viewCenterFreq.value - rfCenterFreq.value);
  rxSampleRate.value = chooseSampleRate(requiredBandwidth);
};

const restartRx = async () => {
  if (!running.value) return;
  await stop();
  await start();
};

const onTuneChange = async () => {
  normalizeTuning();
  await restartRx();
};

const openKeypad = (field: KeypadField) => {
  keypadField.value = field;
  keypadOpenToken.value += 1;
};

const closeKeypad = () => {
  keypadField.value = null;
};

const onKeypadSubmit = async (valueHz: number) => {
  const rounded = Math.round(valueHz);
  if (!Number.isFinite(rounded)) return;

  if (keypadField.value === 'span') {
    spanHz.value = rounded;
  } else if (keypadField.value === 'target') {
    targetFreq.value = rounded;
  }
  closeKeypad();
  await onTuneChange();
};

normalizeTuning();
const saveSettings = () => {
  try {
    const data: PersistedSettings = {
      spanHz: spanHz.value,
      targetFreq: targetFreq.value,
      dcCancelEnabled: dcCancelEnabled.value,
      fftUseProcessed: fftUseProcessed.value,
      ampEnabled: options.ampEnabled,
      antennaEnabled: options.antennaEnabled,
      lnaGain: options.lnaGain,
      vgaGain: options.vgaGain,
      demodMode: demodMode.value,
    };
    localStorage.setItem(settingsStorageKey, JSON.stringify(data));
  } catch {
    // localStorage unavailable/quota exceeded
  }
};
saveSettings();

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

const onDemodModeChange = async () => {
  if (backend && running.value) {
    await restartRx();
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

const stopRenderLoop = () => {
  if (renderLoopId !== null) {
    cancelAnimationFrame(renderLoopId);
    renderLoopId = null;
  }
  renderLastTimeMs = 0;
  latestFftFrame = null;
};

const drawFftAndWaterfall = (
  canvasFftCtx: CanvasRenderingContext2D,
  canvasFft: HTMLCanvasElement,
  fftOut: Float32Array
) => {
  if (waterfall) {
    waterfall.renderLine(fftOut);
  }

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
  const widthHz = spanHz.value;

  const ratio = Math.min(1, Math.max(0, (targetHz - startHz) / widthHz));
  const x = canvasFft.width * ratio;

  canvasFftCtx.beginPath();
  canvasFftCtx.moveTo(x, 0);
  canvasFftCtx.lineTo(x, canvasFft.height);
  canvasFftCtx.strokeStyle = "rgba(255, 0, 0, 0.8)";
  canvasFftCtx.lineWidth = 1;
  canvasFftCtx.stroke();

  canvasFftCtx.restore();
};

const startRenderLoop = (
  canvasFftCtx: CanvasRenderingContext2D,
  canvasFft: HTMLCanvasElement
) => {
  stopRenderLoop();
  const tick = (timeMs: number) => {
    renderLoopId = requestAnimationFrame(tick);

    if (timeMs - renderLastTimeMs < waterfallFrameIntervalMs) return;
    renderLastTimeMs = timeMs;

    if (!latestFftFrame) return;
    drawFftAndWaterfall(canvasFftCtx, canvasFft, latestFftFrame);
  };
  renderLoopId = requestAnimationFrame(tick);
};

const start = async () => {
  if (!connected.value) return;

  normalizeTuning();
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
  startRenderLoop(canvasFftCtx, canvasFft);

  // Comlinkのコールバック関数は proxy に包む必要がある
  const onData = Comlink.proxy((audioOut: Float32Array, fftOut: Float32Array) => {
    playAudioBuffer(audioOut);
    // Wasm側バッファ再利用の影響を避けるためコピーして保持する
    latestFftFrame = new Float32Array(fftOut);
  });

  await backend.startRx({
    sampleRate: rxSampleRate.value,
    centerFreq: rfCenterFreq.value,
    targetFreq: targetFreq.value,
    demodMode: demodMode.value,
    outputSampleRate: audioCtx!.sampleRate,
    fftSize: fftSizeFull,
    fftVisibleStartBin: fftViewWindow.startBin,
    fftVisibleBins,
    ifMinHz: ifMinHz.value,
    ifMaxHz: ifMaxHz.value,
    dcCancelEnabled: dcCancelEnabled.value,
    fftUseProcessed: fftUseProcessed.value,
    ampEnabled: options.ampEnabled,
    antennaEnabled: options.antennaEnabled,
    lnaGain: options.lnaGain,
    vgaGain: options.vgaGain,
  }, onData);

  running.value = true;
};

const stop = async () => {
  if (backend) {
    await backend.stopRx();
  }
  stopRenderLoop();
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
watch(
  [
    spanHz,
    targetFreq,
    dcCancelEnabled,
    fftUseProcessed,
    demodMode,
    () => options.ampEnabled,
    () => options.antennaEnabled,
    () => options.lnaGain,
    () => options.vgaGain,
  ],
  () => { saveSettings(); }
);

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

.field-input-btn {
	width: 100%;
	padding: 8px 12px;
	border: 1px solid #444;
	border-radius: 4px;
	background: #222;
	color: #fff;
	font-size: 14px;
	text-align: left;
	cursor: pointer;
}

.field-input-btn:hover {
	border-color: #666;
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

.dialog-overlay {
	position: fixed;
	inset: 0;
	background: rgba(0, 0, 0, 0.55);
	display: flex;
	align-items: center;
	justify-content: center;
	z-index: 1100;
	padding: 12px;
	box-sizing: border-box;
}

.dialog {
	background: #f9f9f9;
	color: #222;
	border-radius: 8px;
	overflow: hidden;
	box-shadow: 0 12px 28px rgba(0, 0, 0, 0.35);
}

.dialog-title {
	padding: 12px 16px;
	font-weight: 600;
	border-bottom: 1px solid #ddd;
}

.dialog-content {
	padding: 12px 16px;
}

.dialog-actions {
	display: flex;
	justify-content: flex-end;
	padding: 12px 16px;
	border-top: 1px solid #ddd;
}
</style>
