class AudioStreamProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.queue = [];
    this.bufferedSamples = 0;
    this.started = false;
    this.lastSample = 0.0;
    this.hardUnderrunCount = 0;
    this.consecutiveEmptyBlocks = 0;
    this.inHardUnderrun = false;
    // 128sample/block 前提で約350ms。短いフォーカス移動で hard underrun にしない。
    this.maxSoftUnderrunBlocks = Math.max(12, Math.floor((sampleRate * 0.35) / 128));

    // ジッタ吸収用に少しバッファをためてから再生を開始する。
    this.minStartSamples = Math.floor(sampleRate * 0.08);
    this.maxBufferedSamples = Math.floor(sampleRate * 1.8);
    this.targetBufferedSamples = Math.floor(sampleRate * 0.4);
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
        this.lastStatsAt = currentTime;
      }
    };
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

    out.fill(0);

    if (!this.started) {
      if (this.bufferedSamples < this.minStartSamples) {
        return true;
      }
      this.started = true;
    }

    let written = 0;
    while (written < out.length && this.queue.length > 0) {
      const head = this.queue[0];
      const available = head.data.length - head.readPos;
      const take = Math.min(available, out.length - written);

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
    if (written < out.length) {
      this.underrunCount += 1;
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
    }

    if (currentTime - this.lastStatsAt >= 0.5) {
      this.lastStatsAt = currentTime;
      this.port.postMessage({
        type: 'stats',
        bufferedMs: (this.bufferedSamples / sampleRate) * 1000,
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
