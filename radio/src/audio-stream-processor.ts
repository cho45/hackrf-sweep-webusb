/// <reference lib="webworker" />
import {
  RecycleTransferReceiver,
  type RecycleTransferInputMessage,
  type RecycleTransferRecycleMessage,
} from "./recycle-transfer-bridge";

declare abstract class AudioWorkletProcessor {
  readonly port: MessagePort;
  constructor(options?: unknown);
  process(
    inputs: Float32Array[][],
    outputs: Float32Array[][],
    parameters: Record<string, Float32Array>,
  ): boolean;
}

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

const workletGlobal = globalThis as { sampleRate: number; currentTime: number };

export class AudioStreamProcessor extends AudioWorkletProcessor {
  private readonly packetQueue = new RecycleTransferReceiver();

  private inputPort: MessagePort | null = null;

  private started = false;

  private bufferLength = 128;

  private lastSample = 0.0;

  private hardUnderrunCount = 0;

  private consecutiveEmptyBlocks = 0;

  private inHardUnderrun = false;

  private maxSoftUnderrunBlocks = 12;

  private readonly sampleRateHz = workletGlobal.sampleRate;

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
    const msg: RecycleTransferRecycleMessage = { type: "recycle", data };
    this.inputPort.postMessage(msg, [data.buffer]);
  }

  private recycleAllQueuedChunks(): void {
    this.packetQueue.reset((data) => this.recycleChunkData(data));
  }

  constructor() {
    super();
    this.maxSoftUnderrunBlocks = Math.max(
      12,
      Math.floor((this.sampleRateHz * 0.35) / this.bufferLength),
    );
    this.recomputeBufferTargets();
    this.lastStatsAt = workletGlobal.currentTime;

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
          this.handleInputMessage(e.data as RecycleTransferInputMessage);
        this.inputPort.start();
      }
    };
  }

  private handleInputMessage(msg: RecycleTransferInputMessage): void {
    if (!msg || typeof msg !== "object") return;

    if (msg.type === "push") {
      if (!this.packetQueue.pushFromMessage(msg)) return;

      const nowSec = workletGlobal.currentTime;
      if (this.lastPushAtSec >= 0) {
        const gapSec = Math.max(0, nowSec - this.lastPushAtSec);
        if (gapSec > this.pushIntervalPeakSec) {
          this.pushIntervalPeakSec = gapSec;
        }
      }
      this.lastPushAtSec = nowSec;

      const bufferedFrames = this.packetQueue.getBufferedFrames();
      if (bufferedFrames > this.maxBufferedSamples) {
        this.dropOldFrames(bufferedFrames - this.targetBufferedSamples);
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
      this.lastStatsAt = workletGlobal.currentTime;
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
    const nowSec = workletGlobal.currentTime;
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
    this.droppedSamples += this.packetQueue.dropOldFrames(
      framesToDrop,
      (data) => this.recycleChunkData(data),
    );
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
      if (this.packetQueue.getBufferedFrames() < this.minStartSamples) {
        this.postStatsIfNeeded();
        return true;
      }
      this.started = true;
    }

    const drained = this.packetQueue.drainInto(
      outL,
      outR,
      (data) => this.recycleChunkData(data),
    );
    const written = drained.writtenFrames;

    if (written > 0) {
      this.lastSample = drained.lastSample;
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
      if (this.packetQueue.getBufferedFrames() < this.lowWaterSamples) {
        this.started = false;
      }
    }

    this.postStatsIfNeeded();
    return true;
  }

  private postStatsIfNeeded(): void {
    const nowSec = workletGlobal.currentTime;
    if (nowSec - this.lastStatsAt < 0.5) return;

    this.lastStatsAt = nowSec;
    const msg: StatsMessage = {
      type: "stats",
      bufferedMs: (this.packetQueue.getBufferedFrames() / this.sampleRateHz) * 1000,
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
