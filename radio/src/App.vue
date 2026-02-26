<template>
  <div id="app">
    <div class="actions">
      <div style="margin-bottom: 20px;">
        <div class="action-row">
          <button class="btn btn-primary" :disabled="rxTransitioning" v-on:click="start" v-if="!running">Start Rx</button>
          <button class="btn btn-secondary" :disabled="rxTransitioning" v-on:click="stop" v-if="running">Stop Rx</button>
        </div>
        <div class="action-row">
          <button class="btn btn-settings" type="button" :disabled="rxTransitioning" @click="openSettings">Settings</button>
          <button class="btn" :disabled="rxTransitioning || !connected" v-on:click="disconnect">Disconnect</button>
        </div>
      </div>

      <div class="form">
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
          <label>Demod Mode</label>
          <div class="field-input">
            <select v-model="demodMode" @change="onDemodModeChange" style="flex:1; padding:8px 12px; border:1px solid #444; border-radius:4px; background:#222; color:#fff; font-size:14px;">
              <option value="AM">AM</option>
              <option value="FM">FM (WFM)</option>
            </select>
          </div>
        </div>

        <fieldset class="gain-fieldset">
          <legend>Gain</legend>
          <div class="field auto-gain-action">
            <button
              class="btn btn-secondary"
              type="button"
              :disabled="!running || rxTransitioning || autoGainRunning"
              @click="runAutoSetGain"
            >
              {{ autoGainRunning ? 'Auto Setting...' : 'Auto Set Gain' }}
            </button>
            <div class="caption" v-if="autoGainSummary">{{ autoGainSummary }}</div>
            <div class="caption">
              ADC Peak:
              <template v-if="running">
                {{ fmtNum(dspPerf.adcPeak, 0) }} ({{ fmtNum(dspPerf.adcPeak / 127 * 100, 1) }}%)
              </template>
              <template v-else>-</template>
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

          <div class="field">
            <label>LNA Gain (IF)</label>
            <div class="field-input">
              <input type="range" min="0" max="40" step="8" v-model.number="options.lnaGain" class="field-range" />
              <input type="number" min="0" max="40" step="8" v-model.number="options.lnaGain" class="field-number" />
              <span class="field-suffix">dB</span>
            </div>
          </div>

          <label class="checkbox">
            <input type="checkbox" v-model="options.ampEnabled"> RF Amp (14dB)
          </label>
        </fieldset>
      </div>

      <div class="body-2" style="margin-top: 20px;" v-if="connected">
        {{ info.boardName }}<br>
        {{ info.firmwareVersion }}
      </div>
      <div class="perf-panel body-2" v-if="running && showDebugInfo">
        <div><b>DSP</b></div>
        <div>block interval peak: {{ fmtNum(dspPerf.blockIntervalMsPeak, 2) }} ms</div>
        <div>process last/peak: {{ fmtNum(dspPerf.dspProcessMsLast, 2) }} / {{ fmtNum(dspPerf.dspProcessMsPeak, 2) }} ms</div>
        <div>audio out long: {{ fmtNum(dspPerf.audioOutHzLong, 0) }} / {{ fmtNum(audioOutputSampleRate, 0) }} Hz</div>
        <div>dropped IQ blocks: {{ fmtNum(dspPerf.droppedIqBlocksCount, 0) }}</div>
        <div>fft target/noise/snr: {{ fmtNum(dspPerf.fftTargetDb, 1) }} / {{ fmtNum(dspPerf.fftNoiseFloorDb, 1) }} / {{ fmtNum(dspPerf.fftSnrDb, 1) }} dB</div>
        <div>stereo: {{ dspPerf.fmStereoLocked ? 'LOCK' : 'MONO' }} / blend {{ fmtNum(dspPerf.fmStereoBlend, 2) }}</div>
        <div>pilot: {{ fmtNum(dspPerf.fmStereoPilotLevel, 3) }} / mono fallback: {{ fmtNum(dspPerf.fmStereoMonoFallbackCount, 0) }}</div>
        <div>pll: {{ dspPerf.fmStereoPllLocked ? 'LOCK' : 'UNLOCK' }}</div>

        <div style="margin-top: 8px;"><b>Draw</b></div>
        <div>fps: {{ fmtNum(drawPerf.fps, 1) }}</div>
        <div>draw ms(avg/peak): {{ fmtNum(drawPerf.drawMsAvg, 2) }} / {{ fmtNum(drawPerf.drawMsMax, 2) }}</div>

        <div style="margin-top: 8px;"><b>Audio</b></div>
        <div>buffer: {{ fmtNum(audioPerf.bufferedMs, 1) }} ms</div>
        <div>input gap peak: {{ fmtNum(audioPerf.inputGapMsPeak, 2) }} ms</div>
        <div>underrun: {{ fmtNum(audioPerf.underrunCount, 0) }}</div>
        <div>dropped audio: {{ fmtNum(audioPerf.droppedSamplesCount, 0) }} samples</div>
      </div>

      <div class="snackbar" :class="{ show: snackbar.show }">
        {{ snackbar.message }}
      </div>
    </div>

    <div class="dialog-overlay" v-if="settingsOpen" @click.self="closeSettings">
      <div class="settings-dialog">
        <div class="settings-title">Settings</div>
        <div class="settings-content">
          <label class="checkbox">
            <input type="checkbox" v-model="options.antennaEnabled"> Antenna Port Power
          </label>
          <label class="checkbox">
            <input type="checkbox" v-model="dcCancelEnabled"> FFT DC Interpolate
          </label>
          <label class="checkbox">
            <input type="checkbox" v-model="fmStereoEnabled"> FM Stereo
          </label>
          <label class="checkbox">
            <input type="checkbox" v-model="showDebugInfo"> Show Debug Info
          </label>
        </div>
        <div class="settings-actions">
          <button class="btn btn-secondary" type="button" @click="closeSettings">Close</button>
        </div>
      </div>
    </div>
    
    <div
      class="canvas-container"
      ref="canvasContainer"
      @pointermove="onCanvasPointerMove"
      @click="onCanvasClick"
      @pointerleave="hideCanvasPointerFreq"
      @pointercancel="hideCanvasPointerFreq"
    >
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
      <div
        v-if="pointerFreq.visible"
        class="pointer-freq"
        :style="{ left: `${pointerFreq.x}px`, top: `${pointerFreq.y}px` }"
      >
        {{ formatPointerFreq(pointerFreq.hz) }}
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
import { ref, reactive, computed, onUnmounted, watch, nextTick } from 'vue';
import * as Comlink from 'comlink';
import { WaterfallGL, Waterfall } from './utils';
import { HackRF } from './hackrf';
import Keypad from './components/Keypad.vue';
import audioWorkletModuleUrl from './audio-stream-processor.ts?worker&url';

// comlink 経由でバックエンド(WASM/HackRF処理)をロード
const WorkerBackend = Comlink.wrap<any>(new Worker(new URL('./worker.ts', import.meta.url), { type: 'module' }));

const connected = ref(false);
const running = ref(false);
const rxTransitioning = ref(false);
const snackbar = reactive({ show: false, message: '' });

// HackRF Info
const info = reactive({ boardName: '', firmwareVersion: '' });

// 受信パラメータ
const minTuneFreqHz = 1_000_000;
const minDisplayBandwidthHz = 100_000;
const maxHackRFSampleRate = 20_000_000;
const minHackRFSampleRate = 2_000_000;
const rxSampleRateCandidatesHz = [2_000_000, 4_000_000, 8_000_000, 10_000_000, 12_000_000, 16_000_000, 20_000_000] as const;
const ifOffsetHz = 250_000; // target からこの分だけRF centerをずらしてDC回避

const settingsStorageKey = 'radio.settings.v2';
const isDemodMode = (mode: unknown): mode is 'AM' | 'FM' => mode === 'AM' || mode === 'FM';
type PersistedSettings = {
  spanHz: number;
  targetFreq: number;
  dcCancelEnabled: boolean;
  fmStereoEnabled: boolean;
  showDebugInfo: boolean;
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
  fmStereoEnabled: true,
  showDebugInfo: true,
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
      fmStereoEnabled: getBoolean('fmStereoEnabled'),
      showDebugInfo: getBoolean('showDebugInfo'),
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
const fmStereoEnabled = ref(loadedSettings.fmStereoEnabled);
const showDebugInfo = ref(loadedSettings.showDebugInfo);
const demodMode = ref(loadedSettings.demodMode);
const settingsOpen = ref(false);

const defaultIfBandForMode = (mode: string): { minHz: number; maxHz: number } => {
  return mode === 'FM' ? { minHz: 0, maxHz: 98_000 } : { minHz: 0, maxHz: 4_500 };
};

const maxSpanHz = maxHackRFSampleRate;
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
type DspPerfStats = {
  // USB入力欠落が起きていないか
  droppedIqBlocksCount: number;
  // USB/スケジューリング由来の停止スパイク
  blockIntervalMsPeak: number;
  // DSP処理詰まりの検知
  dspProcessMsLast: number;
  // DSP処理詰まりのピーク
  dspProcessMsPeak: number;
  // 長期供給不足の判定
  audioOutHzLong: number;
  // FMステレオ復調状態（AM時はゼロ値）
  fmStereoPilotLevel: number;
  fmStereoBlend: number;
  fmStereoLocked: boolean;
  fmStereoMonoFallbackCount: number;
  fmStereoPllLocked: boolean;
  adcPeak: number;
  fftTargetDb: number;
  fftNoiseFloorDb: number;
  fftSnrDb: number;
};
type AutoGainResult = {
  initialPeak: number;
  finalPeak: number;
  iterations: number;
  appliedSteps: string[];
  ampEnabled: boolean;
  lnaGain: number;
  vgaGain: number;
  settled: boolean;
};
const keypadField = ref<KeypadField | null>(null);
const keypadOpenToken = ref(0);
const keypadPrefillHz = ref<number | null>(null);
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
const keypadSourceHz = computed(() => {
  if (keypadPrefillHz.value !== null) return keypadPrefillHz.value;
  if (keypadField.value === 'span') return spanHz.value;
  return targetFreq.value;
});
const keypadUnit = computed<DisplayUnit>(() => pickDisplayUnit(keypadSourceHz.value));
const keypadInitialValue = computed(() => {
  const factor = unitFactor(keypadUnit.value);
  const precision = keypadUnit.value === 'Hz' ? 0 : 3;
  return (keypadSourceHz.value / factor).toFixed(precision);
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
const audioOutputSampleRate = ref(0);


const waterfallCanvas = ref<HTMLCanvasElement | null>(null);
const fftCanvas = ref<HTMLCanvasElement | null>(null);
const canvasContainer = ref<HTMLDivElement | null>(null);

const pointerFreq = reactive({
  visible: false,
  x: 0,
  y: 0,
  hz: 0,
});

let waterfall: WaterfallGL | Waterfall | null = null;
let latestFftFrame: Float32Array | null = null;
let renderLoopId: number | null = null;
let renderLastTimeMs = 0;
const waterfallFps = 30;
const waterfallFrameIntervalMs = 1000 / waterfallFps;
let drawWindowStartMs = 0;
let drawFrameCount = 0;
let drawMsSum = 0;
let drawMsMax = 0;
const dspPerf = reactive({
  // USB/スケジューリング停止スパイク監視
  blockIntervalMsPeak: 0,
  // DSP過負荷監視
  dspProcessMsLast: 0,
  // DSP過負荷ピーク監視
  dspProcessMsPeak: 0,
  // USB入力欠落監視
  droppedIqBlocksCount: 0,
  // 長期供給不足監視
  audioOutHzLong: 0,
  // FMステレオ復調状態
  fmStereoPilotLevel: 0,
  fmStereoBlend: 0,
  fmStereoLocked: false,
  fmStereoMonoFallbackCount: 0,
  fmStereoPllLocked: false,
  adcPeak: 0,
  fftTargetDb: 0,
  fftNoiseFloorDb: 0,
  fftSnrDb: 0,
});
const drawPerf = reactive({
  fps: 0,
  drawMsAvg: 0,
  drawMsMax: 0,
});
const audioPerf = reactive({
  // 再生余裕（枯渇予兆）
  bufferedMs: 0,
  // 音切れ直接KPI
  underrunCount: 0,
  // 入力停止スパイク
  inputGapMsPeak: 0,
  // バッファ保護のためのサンプル破棄監視
  droppedSamplesCount: 0,
});
const activeRxSampleRate = ref<number | null>(null);
const activeRfCenterFreq = ref<number | null>(null);
const activeFftSize = ref(0);
const activeFftVisibleBins = ref(0);
const autoGainRunning = ref(false);
const autoGainSummary = ref('');
const suppressManualGainSync = ref(false);

const showSnackbar = (msg: string) => {
  snackbar.message = msg;
  snackbar.show = true;
  setTimeout(() => { snackbar.show = false; }, 3000);
};

const fmtNum = (v: number, digits = 2) => Number.isFinite(v) ? v.toFixed(digits) : '-';

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

const formatPointerFreq = (hz: number) => {
  if (Math.abs(hz) >= 1_000_000) return `${(hz / 1_000_000).toFixed(3)} MHz`;
  return `${(hz / 1_000).toFixed(3)} kHz`;
};

const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value));

const pointerLabelYOffset = 25;

const freqAtClientX = (clientX: number) => {
  const container = canvasContainer.value;
  if (!container) return null;
  const rect = container.getBoundingClientRect();
  if (rect.width <= 0) return null;

  const localX = clamp(clientX - rect.left, 0, rect.width);
  const ratio = localX / rect.width;
  return {
    rect,
    localX,
    hz: displayMinFreq.value + spanHz.value * ratio,
  };
};

const onCanvasPointerMove = (event: PointerEvent) => {
  const xInfo = freqAtClientX(event.clientX);
  if (!xInfo) return;
  const { rect, localX, hz } = xInfo;
  const localY = clamp(event.clientY - rect.top, 0, rect.height);

  pointerFreq.visible = true;
  pointerFreq.hz = hz;
  pointerFreq.x = clamp(localX, 56, Math.max(56, rect.width - 56));
  pointerFreq.y = clamp(localY + pointerLabelYOffset, 14, Math.max(14, rect.height - 14));
};

const onCanvasClick = (event: MouseEvent) => {
  const xInfo = freqAtClientX(event.clientX);
  if (!xInfo) return;
  openKeypad('target', xInfo.hz);
};

const hideCanvasPointerFreq = () => {
  pointerFreq.visible = false;
};

const chooseSampleRate = (requiredBandwidth: number) => {
  const required = Math.max(minHackRFSampleRate, requiredBandwidth);
  const selected = rxSampleRateCandidatesHz.find((rate) => rate >= required);
  return selected ?? maxHackRFSampleRate;
};

const normalizeTuning = () => {
  if (targetFreq.value < minTuneFreqHz) targetFreq.value = minTuneFreqHz;
  if (spanHz.value < minDisplayBandwidthHz) spanHz.value = minDisplayBandwidthHz;
  if (spanHz.value > maxSpanHz) spanHz.value = maxSpanHz;

  rxSampleRate.value = chooseSampleRate(spanHz.value);
};

const restartRx = async () => {
  if (!running.value || rxTransitioning.value) return;
  await stop();
  await start();
};

const onTuneChange = async () => {
  normalizeTuning();
  if (!running.value || rxTransitioning.value || !backend) return;

  // サンプルレート変更時だけ full restart する。
  if (activeRxSampleRate.value === null || rxSampleRate.value !== activeRxSampleRate.value) {
    await restartRx();
    return;
  }

  try {
    if (activeRfCenterFreq.value !== rfCenterFreq.value) {
      await backend.setFreq(rfCenterFreq.value);
    }
    await backend.setTargetFreq(rfCenterFreq.value, targetFreq.value);

    if (activeFftSize.value > 0) {
      const fftViewWindow = computeFftViewWindow(activeFftSize.value);
      await backend.setFftView(fftViewWindow.startBin, fftViewWindow.bins);

      const canvasFft = fftCanvas.value;
      const canvasWf = waterfallCanvas.value;
      const canvasFftCtx = canvasFft?.getContext('2d');
      if (canvasFft && canvasWf && canvasFftCtx && activeFftVisibleBins.value !== fftViewWindow.bins) {
        setupFftRendering(canvasFft, canvasWf, canvasFftCtx, fftViewWindow.bins);
      }
      activeFftVisibleBins.value = fftViewWindow.bins;
    }

    activeRfCenterFreq.value = rfCenterFreq.value;
  } catch (e: any) {
    showSnackbar("Retune Error: " + (e?.message ?? String(e)));
    await restartRx();
  }
};

const openKeypad = (field: KeypadField, prefillHz?: number) => {
  keypadPrefillHz.value = typeof prefillHz === 'number' && Number.isFinite(prefillHz)
    ? Math.round(prefillHz)
    : 0;
  keypadField.value = field;
  keypadOpenToken.value += 1;
};

const closeKeypad = () => {
  keypadField.value = null;
  keypadPrefillHz.value = null;
};

const openSettings = () => {
  settingsOpen.value = true;
};

const closeSettings = () => {
  settingsOpen.value = false;
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
      fmStereoEnabled: fmStereoEnabled.value,
      showDebugInfo: showDebugInfo.value,
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

const runAutoSetGain = async () => {
  if (!backend || !running.value || autoGainRunning.value) return;
  autoGainRunning.value = true;
  try {
    const result = await backend.autoSetGainOnce() as AutoGainResult;
    suppressManualGainSync.value = true;
    options.ampEnabled = !!result.ampEnabled;
    options.lnaGain = result.lnaGain;
    options.vgaGain = result.vgaGain;
    await nextTick();
    autoGainSummary.value = `ADC ${result.initialPeak.toFixed(0)} -> ${result.finalPeak.toFixed(0)} / ${result.iterations} steps`;
    showSnackbar(`Auto Set Gain: ${result.settled ? 'settled' : 'partial'}`);
  } catch (e: any) {
    autoGainSummary.value = '';
    showSnackbar("Auto Set Gain Error: " + (e?.message ?? String(e)));
  } finally {
    suppressManualGainSync.value = false;
    autoGainRunning.value = false;
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
  if (rxTransitioning.value) return;
  await backend?.cancelAutoSetGain?.();
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
    await audioCtx.audioWorklet.addModule(audioWorkletModuleUrl);
    audioModuleLoaded = true;
  }

  if (!audioNode) {
    audioNode = new AudioWorkletNode(audioCtx, 'audio-stream-processor', {
      numberOfInputs: 0,
      numberOfOutputs: 1,
      outputChannelCount: [2],
    });
    audioNode.port.onmessage = (event: MessageEvent) => {
      const msg = event.data;
      if (!msg || typeof msg !== 'object' || msg.type !== 'stats') return;
      audioPerf.bufferedMs = typeof msg.bufferedMs === 'number' ? msg.bufferedMs : 0;
      audioPerf.underrunCount = typeof msg.underrunCount === 'number' ? msg.underrunCount : 0;
      audioPerf.inputGapMsPeak = typeof msg.inputGapMsPeak === 'number' ? msg.inputGapMsPeak : 0;
      audioPerf.droppedSamplesCount = typeof msg.droppedSamplesCount === 'number' ? msg.droppedSamplesCount : 0;
    };
    audioNode.connect(audioCtx.destination);
  }

  await audioCtx.resume();
  audioOutputSampleRate.value = audioCtx.sampleRate;
};

const stopAudio = () => {
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
  drawWindowStartMs = 0;
  drawFrameCount = 0;
  drawMsSum = 0;
  drawMsMax = 0;
};

const drawFftAndWaterfall = (
  canvasFftCtx: CanvasRenderingContext2D,
  canvasFft: HTMLCanvasElement,
  fftOut: Float32Array
) => {
  const drawStart = performance.now();
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
  return performance.now() - drawStart;
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
    const drawMs = drawFftAndWaterfall(canvasFftCtx, canvasFft, latestFftFrame);
    if (drawWindowStartMs === 0) drawWindowStartMs = timeMs;
    drawFrameCount += 1;
    drawMsSum += drawMs;
    if (drawMs > drawMsMax) drawMsMax = drawMs;
    const elapsedMs = timeMs - drawWindowStartMs;
    if (elapsedMs >= 1000) {
      const elapsedSec = elapsedMs / 1000;
      drawPerf.fps = drawFrameCount / elapsedSec;
      drawPerf.drawMsAvg = drawMsSum / drawFrameCount;
      drawPerf.drawMsMax = drawMsMax;
      drawWindowStartMs = timeMs;
      drawFrameCount = 0;
      drawMsSum = 0;
      drawMsMax = 0;
    }
  };
  renderLoopId = requestAnimationFrame(tick);
};

const setupFftRendering = (
  canvasFft: HTMLCanvasElement,
  canvasWf: HTMLCanvasElement,
  canvasFftCtx: CanvasRenderingContext2D,
  fftVisibleBins: number
) => {
  canvasFft.width = fftVisibleBins;
  canvasFft.height = 200;

  const maxTextureSize = 16384;
  const useWebGL = fftVisibleBins <= maxTextureSize;
  waterfall = useWebGL
    ? new WaterfallGL(canvasWf, fftVisibleBins, 256)
    : new Waterfall(canvasWf, fftVisibleBins, 256);

  latestFftFrame = null;
  startRenderLoop(canvasFftCtx, canvasFft);
};

const start = async () => {
  if (running.value || rxTransitioning.value) return;
  rxTransitioning.value = true;

  try {
    if (!connected.value) {
      await connect();
      if (!connected.value) return;
    }
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
    setupFftRendering(canvasFft, canvasWf, canvasFftCtx, fftVisibleBins);

    const channel = new MessageChannel();
    audioNode!.port.postMessage({ type: 'attach-input-port', port: channel.port1 }, [channel.port1]);
    await backend.setAudioPort(Comlink.transfer(channel.port2, [channel.port2]));

  // Comlinkのコールバック関数は proxy に包む必要がある
    const onData = Comlink.proxy((fftOut: Float32Array, perf?: DspPerfStats) => {
      if (!latestFftFrame || latestFftFrame.length !== fftOut.length) {
        latestFftFrame = new Float32Array(fftOut.length);
      }
      latestFftFrame.set(fftOut);
      if (perf) {
        dspPerf.blockIntervalMsPeak = perf.blockIntervalMsPeak;
        dspPerf.droppedIqBlocksCount = perf.droppedIqBlocksCount;
        dspPerf.dspProcessMsLast = perf.dspProcessMsLast;
        dspPerf.dspProcessMsPeak = perf.dspProcessMsPeak;
        dspPerf.audioOutHzLong = perf.audioOutHzLong;
        dspPerf.fmStereoPilotLevel = perf.fmStereoPilotLevel;
        dspPerf.fmStereoBlend = perf.fmStereoBlend;
        dspPerf.fmStereoLocked = perf.fmStereoLocked;
        dspPerf.fmStereoMonoFallbackCount = perf.fmStereoMonoFallbackCount;
        dspPerf.fmStereoPllLocked = perf.fmStereoPllLocked;
        dspPerf.adcPeak = perf.adcPeak;
        dspPerf.fftTargetDb = perf.fftTargetDb;
        dspPerf.fftNoiseFloorDb = perf.fftNoiseFloorDb;
        dspPerf.fftSnrDb = perf.fftSnrDb;
      }
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
      fmStereoEnabled: fmStereoEnabled.value,
      ampEnabled: options.ampEnabled,
      antennaEnabled: options.antennaEnabled,
      lnaGain: options.lnaGain,
      vgaGain: options.vgaGain,
    }, onData);

    running.value = true;
    activeRxSampleRate.value = rxSampleRate.value;
    activeRfCenterFreq.value = rfCenterFreq.value;
    activeFftSize.value = fftSizeFull;
    activeFftVisibleBins.value = fftVisibleBins;
  } catch (e: any) {
    stopRenderLoop();
    stopAudio();
    if (backend) {
      try {
        await backend.stopRx();
      } catch (_e) {
        // no-op
      }
    }
    showSnackbar("Start Rx Error: " + (e?.message ?? String(e)));
  } finally {
    rxTransitioning.value = false;
  }
};

const stop = async () => {
  if (!running.value || rxTransitioning.value) return;
  await backend?.cancelAutoSetGain?.();
  rxTransitioning.value = true;
  try {
    if (backend) {
      await backend.stopRx();
    }
  } finally {
    stopRenderLoop();
    stopAudio();
    running.value = false;
    activeRxSampleRate.value = null;
    activeRfCenterFreq.value = null;
    activeFftSize.value = 0;
    activeFftVisibleBins.value = 0;
    rxTransitioning.value = false;
  }
};

// オプションの監視
watch(() => options.lnaGain, (val) => {
  if (suppressManualGainSync.value) return;
  if (connected.value) backend.setLnaGain(val);
});
watch(() => options.vgaGain, (val) => {
  if (suppressManualGainSync.value) return;
  if (connected.value) backend.setVgaGain(val);
});
watch(() => options.ampEnabled, (val) => {
  if (suppressManualGainSync.value) return;
  if (connected.value) backend.setAmpEnable(val);
});
watch(() => options.antennaEnabled, (val) => { if (connected.value) backend.setAntennaEnable(val); });
watch(() => dcCancelEnabled.value, (val) => {
  if (connected.value && running.value) backend.setDcCancelEnabled(val);
});
watch(() => fmStereoEnabled.value, (val) => {
  if (connected.value && running.value) backend.setFmStereoEnabled(val);
});
watch(
  [
    spanHz,
    targetFreq,
    dcCancelEnabled,
    fmStereoEnabled,
    showDebugInfo,
    demodMode,
    () => options.ampEnabled,
    () => options.antennaEnabled,
    () => options.lnaGain,
    () => options.vgaGain,
  ],
  () => { saveSettings(); }
);

onUnmounted(() => {
  void backend?.cancelAutoSetGain?.();
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
	position: relative;
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

.btn-settings {
	background: #37474f;
}

.btn-settings:hover:not(:disabled) {
	background: #455a64;
}

.action-row {
	display: flex;
	align-items: center;
	gap: 8px;
}

.actions .action-row .btn {
	flex: 1;
	margin: 0;
}

.action-row + .action-row {
	margin-top: 8px;
}

/* Form */
.form {
	margin-top: 16px;
}

.gain-fieldset {
	margin: 16px 0 0;
	padding: 12px;
	border: 1px solid #333;
	border-radius: 6px;
}

.gain-fieldset legend {
	padding: 0 6px;
	color: #999;
	font-size: 12px;
}

.auto-gain-action .btn {
	width: 100%;
	margin: 0;
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

.perf-panel {
	margin-top: 12px;
	padding: 8px;
	border: 1px solid #333;
	border-radius: 4px;
	background: #0d0d0d;
	line-height: 1.45;
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
	transform: none;
}

.pointer-freq {
	position: absolute;
	transform: translate(-50%, -50%);
	font-weight: 600;
	font-size: 12px;
	color: #fff;
	background: rgba(0, 0, 0, 0.72);
	border: 1px solid rgba(255, 255, 255, 0.4);
	border-radius: 4px;
	padding: 2px 8px;
	white-space: nowrap;
	pointer-events: none;
	z-index: 20;
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

.settings-dialog {
	width: min(360px, calc(100vw - 24px));
	background: #1a1a1a;
	color: #ddd;
	border-radius: 8px;
	overflow: hidden;
	box-shadow: 0 12px 28px rgba(0, 0, 0, 0.35);
	border: 1px solid #333;
}

.settings-title {
	padding: 12px 16px;
	font-weight: 600;
	border-bottom: 1px solid #333;
}

.settings-content {
	padding: 12px 16px;
}

.settings-actions {
	display: flex;
	justify-content: flex-end;
	padding: 12px 16px;
	border-top: 1px solid #333;
}
</style>
