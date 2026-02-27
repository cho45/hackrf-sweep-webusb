/**
 * AudioPacketSender / AudioPacketReceiver を分けて持つ理由:
 *
 * - Worker 側は WASM 出力を毎ブロック受け取るが、ブロック長は可変になる。
 * - AudioWorklet 側は一定間隔で pull されるため、可変長のまま postMessage すると
 *   メッセージ頻度と割り当てが増え、GC ジッターやスケジューリング遅延で音切れしやすい。
 *
 * このモジュールは、送受信の責務を次のように分離している。
 *
 * 1) AudioPacketSender (Worker 側)
 * - 可変長サンプル列を固定長パケットへ詰め替える。
 * - 送信バッファをプールで保持し、`new Float32Array(...)` の連続生成を避ける。
 * - `postMessage(..., [buffer])` で所有権を転送し、コピーを減らす。
 * - プール枯渇時は残りサンプルを drop し、件数をカウントする。
 *
 * 2) AudioPacketReceiver (AudioWorklet 側)
 * - 受信した固定長パケットをキュー化し、process() 呼び出しごとに drain する。
 * - 消費済みパケットを recycle コールバックで Worker へ返却する。
 *
 * 3) バッファリサイクル
 * - Worker -> Worklet: `push` で transfer 送信 (Worker 側バッファは detached)
 * - Worklet -> Worker: `recycle` で同じバッファを transfer 返却
 * - Worker は返却バッファを sender プールへ戻して再利用
 *
 * この往復で、オーディオ転送パスを「固定長・低アロケーション・所有権再利用」にして
 * バックグラウンド時のジッター耐性を上げる。
 */
export type AudioPushMessage = {
	type: "push";
	data: Float32Array;
	channels?: number;
};

export type AudioRecycleMessage = {
	type: "recycle";
	data: Float32Array;
};

export type AudioInputMessage = AudioPushMessage | { type: "reset" };

type AudioPushPort = {
	postMessage: (message: AudioPushMessage, transfer: Transferable[]) => void;
};

type QueueChunk = {
	data: Float32Array;
	channels: 1 | 2;
	readFrame: number;
};

type DrainResult = {
	writtenFrames: number;
	lastSample: number;
};

export class AudioPacketSender {
	private readonly packetSamples: number;
	private readonly pool: Float32Array[];
	private writeChunk: Float32Array | null = null;
	private writePos = 0;
	private droppedSamplesCount = 0;

	constructor(packetSamples: number, poolSize: number) {
		if (!Number.isFinite(packetSamples) || packetSamples <= 0) {
			throw new Error("packetSamples must be > 0");
		}
		if (!Number.isFinite(poolSize) || poolSize <= 0) {
			throw new Error("poolSize must be > 0");
		}
		this.packetSamples = Math.floor(packetSamples);
		this.pool = [];
		for (let i = 0; i < poolSize; i += 1) {
			this.pool.push(new Float32Array(this.packetSamples));
		}
	}

	getDroppedSamplesCount(): number {
		return this.droppedSamplesCount;
	}

	reset(): void {
		this.pool.length = 0;
		this.writeChunk = null;
		this.writePos = 0;
		this.droppedSamplesCount = 0;
	}

	recycle(chunk: Float32Array): boolean {
		if (!(chunk instanceof Float32Array)) return false;
		if (chunk.length !== this.packetSamples) return false;
		this.pool.push(chunk);
		return true;
	}

	appendFrom(
		src: Float32Array,
		sampleLen: number,
		channels: number,
		port: AudioPushPort | null | undefined
	): void {
		if (!port || sampleLen <= 0) return;
		let srcPos = 0;
		while (srcPos < sampleLen) {
			if (!this.writeChunk) {
				const next = this.pool.pop();
				if (!next) {
					this.droppedSamplesCount += sampleLen - srcPos;
					break;
				}
				this.writeChunk = next;
				this.writePos = 0;
			}
			const dst = this.writeChunk;
			if (!dst) break;
			const remain = this.packetSamples - this.writePos;
			const take = Math.min(remain, sampleLen - srcPos);
			dst.set(src.subarray(srcPos, srcPos + take), this.writePos);
			this.writePos += take;
			srcPos += take;
			if (this.writePos >= this.packetSamples) {
				port.postMessage(
					{ type: "push", channels, data: dst },
					[dst.buffer]
				);
				this.writeChunk = null;
				this.writePos = 0;
			}
		}
	}
}

export class AudioPacketReceiver {
	private chunks: QueueChunk[] = [];
	private head = 0;
	private bufferedFrameCount = 0;

	getBufferedFrames(): number {
		return this.bufferedFrameCount;
	}

	reset(recycle: (data: Float32Array) => void): void {
		while (this.head < this.chunks.length) {
			const chunk = this.chunks[this.head];
			this.head += 1;
			if (chunk) recycle(chunk.data);
		}
		this.compactIfNeeded(true);
		this.bufferedFrameCount = 0;
	}

	pushFromMessage(message: AudioPushMessage): boolean {
		const data = this.normalizeFloat32((message as { data: unknown }).data);
		if (!data || data.length === 0) return false;
		const channels = message.channels === 2 ? 2 : 1;
		if (data.length % channels !== 0) return false;
		const chunk: QueueChunk = {
			data,
			channels,
			readFrame: 0,
		};
		this.chunks.push(chunk);
		this.bufferedFrameCount += data.length / channels;
		return true;
	}

	private normalizeFloat32(value: unknown): Float32Array | null {
		if (value instanceof Float32Array) return value;
		if (value instanceof ArrayBuffer) return new Float32Array(value);
		if (ArrayBuffer.isView(value)) {
			const view = value as ArrayBufferView;
			return new Float32Array(view.buffer, view.byteOffset, Math.floor(view.byteLength / 4));
		}
		return null;
	}

	dropOldFrames(framesToDrop: number, recycle: (data: Float32Array) => void): number {
		let remaining = Math.max(0, Math.floor(framesToDrop));
		let droppedSamples = 0;
		while (remaining > 0 && this.head < this.chunks.length) {
			const head = this.chunks[this.head];
			if (!head) break;
			const available = head.data.length / head.channels - head.readFrame;
			if (available <= remaining) {
				remaining -= available;
				this.bufferedFrameCount -= available;
				droppedSamples += available * head.channels;
				recycle(head.data);
				this.head += 1;
			} else {
				head.readFrame += remaining;
				this.bufferedFrameCount -= remaining;
				droppedSamples += remaining * head.channels;
				remaining = 0;
			}
		}
		this.compactIfNeeded(false);
		return droppedSamples;
	}

	drainInto(
		outL: Float32Array,
		outR: Float32Array,
		recycle: (data: Float32Array) => void
	): DrainResult {
		const frames = outL.length;
		let written = 0;
		let lastSample = 0.0;

		while (written < frames && this.head < this.chunks.length) {
			const head = this.chunks[this.head];
			if (!head) break;

			const availableFrames = head.data.length / head.channels - head.readFrame;
			const takeFrames = Math.min(availableFrames, frames - written);

			if (head.channels === 1) {
				const src = head.readFrame;
				for (let i = 0; i < takeFrames; i += 1) {
					const v = head.data[src + i] ?? 0;
					outL[written + i] = v;
					outR[written + i] = v;
					lastSample = v;
				}
			} else {
				const src = head.readFrame * 2;
				for (let i = 0; i < takeFrames; i += 1) {
					const base = src + i * 2;
					const lv = head.data[base] ?? 0;
					const rv = head.data[base + 1] ?? 0;
					outL[written + i] = lv;
					outR[written + i] = rv;
					lastSample = lv;
				}
			}

			head.readFrame += takeFrames;
			written += takeFrames;
			this.bufferedFrameCount -= takeFrames;
			if (head.readFrame >= head.data.length / head.channels) {
				recycle(head.data);
				this.head += 1;
			}
		}

		this.compactIfNeeded(false);
		return { writtenFrames: written, lastSample };
	}

	private compactIfNeeded(force: boolean): void {
		if (this.head === 0) return;
		if (!force && this.head < 64 && this.head * 2 < this.chunks.length) return;
		this.chunks = this.chunks.slice(this.head);
		this.head = 0;
	}
}
