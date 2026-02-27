/// <reference lib="webworker" />

declare abstract class AudioWorkletProcessor {
  readonly port: MessagePort;
  constructor(options?: unknown);
  process(
    inputs: Float32Array[][],
    outputs: Float32Array[][],
    parameters: Record<string, Float32Array>,
  ): boolean;
}

type QueueChunk = {
  data: Float32Array;
  channels: 1 | 2;
  readFrame: number;
};

type InputMessage =
  | { type: "push"; data: Float32Array; channels?: number }
  | { type: "reset" };

type OutputMessage = {
  type: "recycle";
  data: Float32Array;
};

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

  private bufferedFrames = 0;

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

  private recycleChunkData(data: Float32Array): void {
    if (!this.inputPort) return;
    const msg: OutputMessage = { type: "recycle", data };
    this.inputPort.postMessage(msg, [data.buffer]);
  }

  private recycleAllQueuedChunks(): void {
    while (this.queue.length > 0) {
      const chunk = this.queue.shift();
      if (!chunk) break;
      this.recycleChunkData(chunk.data);
    }
    this.bufferedFrames = 0;
  }

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
          this.recycleAllQueuedChunks();
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

      const channels = msg.channels === 2 ? 2 : 1;
      if (data.length % channels !== 0) return;

      const nowSec = readCurrentTime();
      if (this.lastPushAtSec >= 0) {
        const gapSec = Math.max(0, nowSec - this.lastPushAtSec);
        if (gapSec > this.pushIntervalPeakSec) {
          this.pushIntervalPeakSec = gapSec;
        }
      }
      this.lastPushAtSec = nowSec;

      const frames = data.length / channels;
      this.queue.push({ data, channels, readFrame: 0 });
      this.bufferedFrames += frames;

      if (this.bufferedFrames > this.maxBufferedSamples) {
        this.dropOldFrames(this.bufferedFrames - this.targetBufferedSamples);
      }
      return;
    }

    if (msg.type === "reset") {
      this.recycleAllQueuedChunks();
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

  private dropOldFrames(framesToDrop: number): void {
    let remaining = framesToDrop;
    while (remaining > 0 && this.queue.length > 0) {
      const head = this.queue[0];
      if (!head) break;
      const available = head.data.length / head.channels - head.readFrame;
      if (available <= remaining) {
        remaining -= available;
        this.bufferedFrames -= available;
        this.droppedSamples += available * head.channels;
        this.recycleChunkData(head.data);
        this.queue.shift();
      } else {
        head.readFrame += remaining;
        this.bufferedFrames -= remaining;
        this.droppedSamples += remaining * head.channels;
        remaining = 0;
      }
    }
  }

  process(_inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
    const outputBus = outputs[0];
    const outL = outputBus?.[0];
    if (!outL) return true;
    const outR = outputBus?.[1] ?? outL;

    const bufferLength = outL.length;
    if (this.bufferLength !== bufferLength) {
      this.bufferLength = bufferLength;
      this.maxSoftUnderrunBlocks = Math.max(
        12,
        Math.floor((this.sampleRateHz * 0.35) / this.bufferLength),
      );
    }

    outL.fill(0);
    if (outR !== outL) {
      outR.fill(0);
    }

    if (!this.started) {
      if (this.bufferedFrames < this.minStartSamples) {
        this.postStatsIfNeeded();
        return true;
      }
      this.started = true;
    }

    let written = 0;
    while (written < bufferLength && this.queue.length > 0) {
      const head = this.queue[0];
      if (!head) break;

      const availableFrames = head.data.length / head.channels - head.readFrame;
      const takeFrames = Math.min(availableFrames, bufferLength - written);

      if (head.channels === 1) {
        const src = head.readFrame;
        for (let i = 0; i < takeFrames; i += 1) {
          const v = head.data[src + i] ?? 0;
          outL[written + i] = v;
          outR[written + i] = v;
        }
      } else {
        const src = head.readFrame * 2;
        for (let i = 0; i < takeFrames; i += 1) {
          const base = src + i * 2;
          outL[written + i] = head.data[base] ?? 0;
          outR[written + i] = head.data[base + 1] ?? 0;
        }
      }

      head.readFrame += takeFrames;
      written += takeFrames;
      this.bufferedFrames -= takeFrames;
      if (head.readFrame >= head.data.length / head.channels) {
        this.recycleChunkData(head.data);
        this.queue.shift();
      }
    }

    if (written > 0) {
      this.lastSample = outL[written - 1] ?? this.lastSample;
      this.consecutiveEmptyBlocks = 0;
      this.inHardUnderrun = false;
    }

    if (written < bufferLength) {
      this.underrunCount += 1;
      this.growBufferOnUnderrun();
      if (written === 0) {
        this.consecutiveEmptyBlocks += 1;
        const fill = this.inHardUnderrun ? 0.0 : this.lastSample;
        outL.fill(fill, written);
        outR.fill(fill, written);
        if (this.consecutiveEmptyBlocks > this.maxSoftUnderrunBlocks) {
          if (!this.inHardUnderrun) {
            this.hardUnderrunCount += 1;
            this.inHardUnderrun = true;
          }
        }
      } else {
        outL.fill(this.lastSample, written);
        outR.fill(this.lastSample, written);
      }
      if (this.bufferedFrames < this.lowWaterSamples) {
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
      bufferedMs: (this.bufferedFrames / this.sampleRateHz) * 1000,
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
