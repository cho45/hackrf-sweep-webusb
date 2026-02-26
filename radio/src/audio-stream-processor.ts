type QueueChunk = {
  data: Float32Array;
  readPos: number;
};

type InputMessage =
  | { type: "push"; data: Float32Array }
  | { type: "reset" };

type WorkletControlMessage = {
  type: "attach-input-port";
  port: MessagePort;
};

type StatsMessage = {
  type: "stats";
  bufferedMs: number;
  underrunCount: number;
  droppedSamplesCount: number;
  inputGapMsPeak: number;
};

const readSampleRate = (): number => {
  const sr = (globalThis as { sampleRate?: number }).sampleRate;
  if (typeof sr === "number" && Number.isFinite(sr) && sr > 0) {
    return sr;
  }
  return 48_000;
};

const readCurrentTime = (): number => {
  const t = (globalThis as { currentTime?: number }).currentTime;
  if (typeof t === "number" && Number.isFinite(t)) {
    return t;
  }
  return 0;
};

export class AudioStreamProcessor extends AudioWorkletProcessor {
  private queue: QueueChunk[] = [];

  private inputPort: MessagePort | null = null;

  private bufferedSamples = 0;

  private started = false;

  private bufferLength = 128;

  private lastSample = 0.0;

  private hardUnderrunCount = 0;

  private consecutiveEmptyBlocks = 0;

  private inHardUnderrun = false;

  private maxSoftUnderrunBlocks = 12;

  private readonly sampleRateHz = readSampleRate();

  private baseMinStartSamples = Math.floor(this.sampleRateHz * 0.1);

  private baseLowWaterSamples = Math.floor(this.sampleRateHz * 0.03);

  private baseMaxBufferedSamples = Math.floor(this.sampleRateHz * 1.8);

  private baseTargetBufferedSamples = Math.floor(this.sampleRateHz * 0.4);

  private bufferScale = 1.0;

  private readonly maxBufferScale = 3.0;

  private lastBufferGrowAt = -1;

  private readonly bufferGrowCooldownSec = 0.25;

  private minStartSamples = this.baseMinStartSamples;

  private lowWaterSamples = this.baseLowWaterSamples;

  private targetBufferedSamples = this.baseTargetBufferedSamples;

  private maxBufferedSamples = this.baseMaxBufferedSamples;

  private droppedSamples = 0;

  private underrunCount = 0;

  private lastStatsAt = 0;

  private pushIntervalPeakSec = 0;

  private lastPushAtSec = -1;

  constructor() {
    super();
    this.maxSoftUnderrunBlocks = Math.max(
      12,
      Math.floor((this.sampleRateHz * 0.35) / this.bufferLength),
    );
    this.recomputeBufferTargets();
    this.lastStatsAt = readCurrentTime();

    this.port.onmessage = (event: MessageEvent) => {
      const msg = event.data as WorkletControlMessage | null;
      if (!msg || typeof msg !== "object") return;
      if (
        msg.type === "attach-input-port" &&
        msg.port &&
        typeof msg.port.postMessage === "function"
      ) {
        if (this.inputPort) {
          this.inputPort.close();
        }
        this.inputPort = msg.port;
        this.inputPort.onmessage = (e: MessageEvent) =>
          this.handleInputMessage(e.data as InputMessage);
        this.inputPort.start();
      }
    };
  }

  private handleInputMessage(msg: InputMessage): void {
    if (!msg || typeof msg !== "object") return;

    if (msg.type === "push") {
      const { data } = msg;
      if (!(data instanceof Float32Array) || data.length === 0) return;

      const nowSec = readCurrentTime();
      if (this.lastPushAtSec >= 0) {
        const gapSec = Math.max(0, nowSec - this.lastPushAtSec);
        if (gapSec > this.pushIntervalPeakSec) {
          this.pushIntervalPeakSec = gapSec;
        }
      }
      this.lastPushAtSec = nowSec;

      this.queue.push({ data, readPos: 0 });
      this.bufferedSamples += data.length;

      if (this.bufferedSamples > this.maxBufferedSamples) {
        this.dropOldSamples(this.bufferedSamples - this.targetBufferedSamples);
      }
      return;
    }

    if (msg.type === "reset") {
      this.queue = [];
      this.bufferedSamples = 0;
      this.started = false;
      this.lastSample = 0.0;
      this.droppedSamples = 0;
      this.underrunCount = 0;
      this.hardUnderrunCount = 0;
      this.consecutiveEmptyBlocks = 0;
      this.inHardUnderrun = false;
      this.bufferScale = 1.0;
      this.recomputeBufferTargets();
      this.lastBufferGrowAt = -1;
      this.lastStatsAt = readCurrentTime();
      this.pushIntervalPeakSec = 0;
      this.lastPushAtSec = -1;
    }
  }

  private recomputeBufferTargets(): void {
    this.minStartSamples = Math.floor(this.baseMinStartSamples * this.bufferScale);
    this.lowWaterSamples = Math.floor(this.baseLowWaterSamples * this.bufferScale);
    this.targetBufferedSamples = Math.floor(
      this.baseTargetBufferedSamples * this.bufferScale,
    );
    this.maxBufferedSamples = Math.max(
      this.targetBufferedSamples + this.minStartSamples,
      Math.floor(this.baseMaxBufferedSamples * this.bufferScale),
    );
  }

  private growBufferOnUnderrun(): void {
    if (this.bufferScale >= this.maxBufferScale) return;
    const nowSec = readCurrentTime();
    if (
      this.lastBufferGrowAt >= 0 &&
      nowSec - this.lastBufferGrowAt < this.bufferGrowCooldownSec
    ) {
      return;
    }
    this.bufferScale = Math.min(this.maxBufferScale, this.bufferScale * 1.15);
    this.recomputeBufferTargets();
    this.lastBufferGrowAt = nowSec;
  }

  private dropOldSamples(samplesToDrop: number): void {
    let remaining = samplesToDrop;
    while (remaining > 0 && this.queue.length > 0) {
      const head = this.queue[0];
      if (!head) break;
      const available = head.data.length - head.readPos;
      if (available <= remaining) {
        remaining -= available;
        this.bufferedSamples -= available;
        this.droppedSamples += available;
        this.queue.shift();
      } else {
        head.readPos += remaining;
        this.bufferedSamples -= remaining;
        this.droppedSamples += remaining;
        remaining = 0;
      }
    }
  }

  process(_inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
    const out = outputs[0]?.[0];
    if (!out) return true;

    const bufferLength = out.length;
    if (this.bufferLength !== bufferLength) {
      this.bufferLength = bufferLength;
      this.maxSoftUnderrunBlocks = Math.max(
        12,
        Math.floor((this.sampleRateHz * 0.35) / this.bufferLength),
      );
    }

    out.fill(0);

    if (!this.started) {
      if (this.bufferedSamples < this.minStartSamples) {
        this.postStatsIfNeeded();
        return true;
      }
      this.started = true;
    }

    let written = 0;
    while (written < bufferLength && this.queue.length > 0) {
      const head = this.queue[0];
      if (!head) break;
      const available = head.data.length - head.readPos;
      const take = Math.min(available, bufferLength - written);
      out.set(head.data.subarray(head.readPos, head.readPos + take), written);
      head.readPos += take;
      written += take;
      this.bufferedSamples -= take;
      if (head.readPos >= head.data.length) {
        this.queue.shift();
      }
    }

    if (written > 0) {
      this.lastSample = out[written - 1] ?? this.lastSample;
      this.consecutiveEmptyBlocks = 0;
      this.inHardUnderrun = false;
    }

    if (written < bufferLength) {
      this.underrunCount += 1;
      this.growBufferOnUnderrun();
      if (written === 0) {
        this.consecutiveEmptyBlocks += 1;
        out.fill(this.inHardUnderrun ? 0.0 : this.lastSample, written);
        if (this.consecutiveEmptyBlocks > this.maxSoftUnderrunBlocks) {
          if (!this.inHardUnderrun) {
            this.hardUnderrunCount += 1;
            this.inHardUnderrun = true;
          }
        }
      } else {
        out.fill(this.lastSample, written);
      }
      if (this.bufferedSamples < this.lowWaterSamples) {
        this.started = false;
      }
    }

    this.postStatsIfNeeded();
    return true;
  }

  private postStatsIfNeeded(): void {
    const nowSec = readCurrentTime();
    if (nowSec - this.lastStatsAt < 0.5) return;

    this.lastStatsAt = nowSec;
    const msg: StatsMessage = {
      type: "stats",
      bufferedMs: (this.bufferedSamples / this.sampleRateHz) * 1000,
      underrunCount: this.underrunCount,
      droppedSamplesCount: this.droppedSamples,
      inputGapMsPeak: this.pushIntervalPeakSec * 1000,
    };
    this.port.postMessage(msg);
  }
}

const maybeRegister = (
  globalThis as unknown as {
    registerProcessor?: (
      name: string,
      ctor: new () => AudioWorkletProcessor,
    ) => void;
  }
).registerProcessor;

if (typeof maybeRegister === "function") {
  maybeRegister("audio-stream-processor", AudioStreamProcessor);
}
