class AudioStreamProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.queue = [];
    this.bufferedSamples = 0;
    this.started = false;

    // ジッタ吸収用に少しバッファをためてから再生を開始する。
    this.minStartSamples = Math.floor(sampleRate * 0.08);
    this.maxBufferedSamples = Math.floor(sampleRate * 1.5);
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
        this.droppedSamples = 0;
        this.underrunCount = 0;
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

    // 供給不足時は再バッファリングへ戻る。
    if (written < out.length) {
      this.started = false;
      this.underrunCount += 1;
    }

    if (currentTime - this.lastStatsAt >= 0.5) {
      this.lastStatsAt = currentTime;
      this.port.postMessage({
        type: 'stats',
        bufferedMs: (this.bufferedSamples / sampleRate) * 1000,
        queueLength: this.queue.length,
        droppedSamples: this.droppedSamples,
        underrunCount: this.underrunCount,
      });
    }

    return true;
  }
}

registerProcessor('audio-stream-processor', AudioStreamProcessor);
