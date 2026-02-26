class AudioStreamProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.queue = [];
    this.bufferedSamples = 0;
    this.started = false;
    this.bufferLength = 128;
    this.lastSample = 0.0;
    this.hardUnderrunCount = 0;
    this.consecutiveEmptyBlocks = 0;
    this.inHardUnderrun = false;
    // 約350ms程度は hard underrun 扱いにしない（block size 依存）。
    this.maxSoftUnderrunBlocks = Math.max(12, Math.floor((sampleRate * 0.35) / this.bufferLength));

    // バッファ方針（将来はUI設定値で上書きできるよう、基準値を分離して保持）
    this.baseMinStartSamples = Math.floor(sampleRate * 0.10);
    this.baseLowWaterSamples = Math.floor(sampleRate * 0.03);
    this.baseMaxBufferedSamples = Math.floor(sampleRate * 1.8);
    this.baseTargetBufferedSamples = Math.floor(sampleRate * 0.4);
    this.bufferScale = 1.0;
    this.maxBufferScale = 3.0;
    this.lastBufferGrowAt = -1;
    this.bufferGrowCooldownSec = 0.25;
    this.recomputeBufferTargets();

    this.droppedSamples = 0;
    this.underrunCount = 0;
    this.lastStatsAt = currentTime;

    this.port.onmessage = (event) => {
      const msg = event.data;
      if (!msg || typeof msg !== 'object') return;

      if (msg.type === 'push') {
        const data = msg.data;
        if (!(data instanceof Float32Array) || data.length === 0) return;

        this.queue.push({ data, readPos: 0 });
        this.bufferedSamples += data.length;

        // 過大遅延を避けるため、古いデータを破棄して目標遅延へ戻す。
        if (this.bufferedSamples > this.maxBufferedSamples) {
          this.dropOldSamples(this.bufferedSamples - this.targetBufferedSamples);
        }
      } else if (msg.type === 'reset') {
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
        this.lastStatsAt = currentTime;
      }
    };
  }

  recomputeBufferTargets() {
    this.minStartSamples = Math.floor(this.baseMinStartSamples * this.bufferScale);
    this.lowWaterSamples = Math.floor(this.baseLowWaterSamples * this.bufferScale);
    this.targetBufferedSamples = Math.floor(this.baseTargetBufferedSamples * this.bufferScale);
    this.maxBufferedSamples = Math.max(
      this.targetBufferedSamples + this.minStartSamples,
      Math.floor(this.baseMaxBufferedSamples * this.bufferScale),
    );
  }

  growBufferOnUnderrun() {
    if (this.bufferScale >= this.maxBufferScale) return;
    if (
      this.lastBufferGrowAt >= 0 &&
      currentTime - this.lastBufferGrowAt < this.bufferGrowCooldownSec
    ) {
      return;
    }
    this.bufferScale = Math.min(this.maxBufferScale, this.bufferScale * 1.15);
    this.recomputeBufferTargets();
    this.lastBufferGrowAt = currentTime;
  }

  dropOldSamples(samplesToDrop) {
    let remaining = samplesToDrop;
    while (remaining > 0 && this.queue.length > 0) {
      const head = this.queue[0];
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

  process(_inputs, outputs) {
    const out = outputs[0][0];
    if (!out) {
      return true;
    }
    const bufferLength = out.length;
    if (this.bufferLength !== bufferLength) {
      this.bufferLength = bufferLength;
      this.maxSoftUnderrunBlocks = Math.max(
        12,
        Math.floor((sampleRate * 0.35) / this.bufferLength),
      );
    }

    out.fill(0);

    if (!this.started) {
      if (this.bufferedSamples < this.minStartSamples) {
        return true;
      }
      this.started = true;
    }

    let written = 0;
    while (written < bufferLength && this.queue.length > 0) {
      const head = this.queue[0];
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
      this.lastSample = out[written - 1];
      this.consecutiveEmptyBlocks = 0;
      this.inHardUnderrun = false;
    }

    // 供給不足時はサンプルホールドで短期ジッタを吸収する。
    if (written < bufferLength) {
      this.underrunCount += 1;
      this.growBufferOnUnderrun();
      if (written === 0) {
        this.consecutiveEmptyBlocks += 1;
        out.fill(this.inHardUnderrun ? 0.0 : this.lastSample, written);
        // 完全枯渇が一定時間以上続いた場合は hard underrun として記録。
        // 再バッファリング状態へ戻さず、復帰時は即座に再生を再開する。
        if (this.consecutiveEmptyBlocks > this.maxSoftUnderrunBlocks) {
          if (!this.inHardUnderrun) {
            this.hardUnderrunCount += 1;
            this.inHardUnderrun = true;
          }
        }
      } else {
        out.fill(this.lastSample, written);
      }

      // 一度薄くなった状態で継続再生すると underrun が連鎖しやすいため、
      // 低水位まで落ちたら再バッファリングに戻す。
      if (this.bufferedSamples < this.lowWaterSamples) {
        this.started = false;
      }
    }

    if (currentTime - this.lastStatsAt >= 0.5) {
      this.lastStatsAt = currentTime;
      this.port.postMessage({
        type: 'stats',
        bufferedMs: (this.bufferedSamples / sampleRate) * 1000,
        minStartMs: (this.minStartSamples / sampleRate) * 1000,
        lowWaterMs: (this.lowWaterSamples / sampleRate) * 1000,
        bufferScale: this.bufferScale,
        queueLength: this.queue.length,
        droppedSamples: this.droppedSamples,
        underrunCount: this.underrunCount,
        hardUnderrunCount: this.hardUnderrunCount,
      });
    }

    return true;
  }
}

registerProcessor('audio-stream-processor', AudioStreamProcessor);
