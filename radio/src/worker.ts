import { expose } from "comlink";
import initBase, { Receiver as ReceiverBase } from "../hackrf-dsp/pkg/hackrf_dsp";
import initSimd, { Receiver as ReceiverSimd } from "../hackrf-dsp/pkg-simd/hackrf_dsp";
import { HackRF } from "./hackrf";

type PerfStats = {
	// USB入力欠落が起きていないか（受信パス健全性）
	droppedIqBlocksCount: number;
	// USB/スケジューリング由来の停止スパイク検知
	blockIntervalMsPeak: number;
	// DSP処理が詰まり要因になっていないか
	dspProcessMsPeak: number;
	// 長期の供給不足判定（短窓の揺れは見ない）
	audioOutHzLong: number;
	// FMステレオ復調状態（AM時はゼロ値）
	pilotLevel: number;
	stereoBlend: number;
	stereoLocked: boolean;
	monoFallbackCount: number;
};

type WasmInitFn = () => Promise<any>;
type ReceiverCtor = new (
	sampleRate: number,
	centerFreq: number,
	targetFreq: number,
	demodMode: string,
	outputSampleRate: number,
	fftSize: number,
	fftVisibleStartBin: number,
	fftVisibleBins: number,
	ifMinHz: number,
	ifMaxHz: number,
	dcCancelEnabled: boolean
) => {
	alloc_io_buffers: (maxIqBytes: number, maxAudioSamples: number, maxFftBins: number) => Promise<void> | void;
	free_io_buffers: () => void;
	iq_input_ptr: () => number;
	audio_output_ptr: () => number;
	fft_output_ptr: () => number;
	iq_input_capacity: () => number;
	audio_output_capacity: () => number;
	fft_output_capacity: () => number;
	process_iq_len: (iqLen: number) => number;
	audio_output_channels: () => number;
	get_stats: () => unknown;
	free: () => void;
	set_target_freq: (centerFreq: number, targetFreq: number) => void;
	set_if_band: (minHz: number, maxHz: number) => void;
	set_dc_cancel_enabled: (enabled: boolean) => void;
};

type WasmBindings = {
	init: WasmInitFn;
	Receiver: ReceiverCtor;
	flavor: "simd" | "base";
};

const SIMD_PROBE_WASM = new Uint8Array([
	0x00, 0x61, 0x73, 0x6d, // magic
	0x01, 0x00, 0x00, 0x00, // version
	0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, // type: (func (result i32))
	0x03, 0x02, 0x01, 0x00, // function section
	0x07, 0x05, 0x01, 0x01, 0x66, 0x00, 0x00, // export "f"
	0x0a, 0x19, 0x01, 0x17, 0x00, // code: 1 func, body size=23, local decl=0
	0xfd, 0x0c, // v128.const
	0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
	0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // 16-byte lane data
	0xfd, 0x15, 0x00, // i8x16.extract_lane_s 0
	0x0b, // end
]);

const supportsWasmSimd = () => {
	if (typeof WebAssembly === "undefined" || typeof WebAssembly.validate !== "function") {
		return false;
	}
	try {
		return WebAssembly.validate(SIMD_PROBE_WASM);
	} catch {
		return false;
	}
};

const BASE_BINDINGS: WasmBindings = {
	init: initBase as WasmInitFn,
	Receiver: ReceiverBase as unknown as ReceiverCtor,
	flavor: "base",
};

const SIMD_BINDINGS: WasmBindings = {
	init: initSimd as WasmInitFn,
	Receiver: ReceiverSimd as unknown as ReceiverCtor,
	flavor: "simd",
};

export class RadioBackend {
	device?: HackRF;
	receiver?: InstanceType<ReceiverCtor>;
	wasmModule?: any;
	private wasmBindings?: WasmBindings;
	private audioPort?: MessagePort;
	private ampEnabled = false;
	private antennaEnabled = false;
	private lnaGain = 16;
	private vgaGain = 16;

	private async ensureWasm() {
		if (!this.wasmBindings) {
			this.wasmBindings = supportsWasmSimd() ? SIMD_BINDINGS : BASE_BINDINGS;
		}
		if (!this.wasmModule) {
			if (this.wasmBindings.flavor === "simd") {
				try {
					this.wasmModule = await this.wasmBindings.init();
				} catch (e) {
					console.warn("[radio] failed to initialize SIMD wasm; falling back to base wasm", e);
					this.wasmBindings = BASE_BINDINGS;
					this.wasmModule = await this.wasmBindings.init();
				}
			} else {
				this.wasmModule = await this.wasmBindings.init();
			}
			console.info(`[radio] loaded wasm flavor: ${this.wasmBindings.flavor}`);
		}
	}

	async init() {
		await this.ensureWasm();
		return true;
	}

	async open(opts?: { vendorId?: number, productId?: number, serialNumber?: string }) {
		const devices = await navigator.usb.getDevices();
		const device = !opts ? devices[0] : devices.find(d => {
			if (opts.vendorId && d.vendorId !== opts.vendorId) return false;
			if (opts.productId && d.productId !== opts.productId) return false;
			if (opts.serialNumber && d.serialNumber !== opts.serialNumber) return false;
			return true;
		});
		if (!device) return false;

		this.device = new HackRF();
		await this.device.open(device);
		// 既存セッションの取りこぼし状態を避けるため、接続直後に必ずRX OFFに戻す。
		await this.device.stopRx();
		return true;
	}

	async close() {
		if (this.device) {
			await this.device.close();
			await this.device.exit();
			this.device = undefined;
		}
	}

	async info() {
		if (!this.device) throw new Error("not connected");

		const boardId = await this.device.readBoardId();
		const versionString = await this.device.readVersionString();
		const apiVersion = await this.device.readApiVersion();
		const { partId, serialNo } = await this.device.readPartIdSerialNo();

		return { boardId, versionString, apiVersion, partId, serialNo };
	}

	async setAmpEnable(val: boolean) {
		this.ampEnabled = val;
		if (this.device) await this.device.setAmpEnable(val);
	}

	async setAntennaEnable(val: boolean) {
		this.antennaEnabled = val;
		if (this.device) await this.device.setAntennaEnable(val);
	}

	async setLnaGain(val: number) {
		this.lnaGain = val;
		if (this.device) await this.device.setLnaGain(val);
	}

	async setVgaGain(val: number) {
		this.vgaGain = val;
		if (this.device) await this.device.setVgaGain(val);
	}

	async setFreq(centerFreq: number) {
		if (this.device) await this.device.setFreq(centerFreq);
	}

	async setAudioPort(port: MessagePort) {
		if (this.audioPort) {
			try {
				this.audioPort.close();
			} catch (_e) {
				// no-op
			}
		}
		this.audioPort = port;
		if (typeof this.audioPort.start === "function") {
			this.audioPort.start();
		}
	}

	async startRx(
		options: {
			sampleRate: number;
			centerFreq: number;
			targetFreq: number;
			demodMode: string;
			outputSampleRate: number;
			fftSize: number;
			fftVisibleStartBin: number;
			fftVisibleBins: number;
			ifMinHz: number;
			ifMaxHz: number;
			dcCancelEnabled: boolean;
			ampEnabled: boolean;
			antennaEnabled: boolean;
				lnaGain: number;
				vgaGain: number;
			},
			onData: (fftOut: Float32Array, perf?: PerfStats) => void
	) {
		if (!this.device) throw new Error("device not opened");
		await this.ensureWasm();
		if (!this.wasmBindings) throw new Error("wasm bindings are not initialized");
		this.ampEnabled = options.ampEnabled;
		this.antennaEnabled = options.antennaEnabled;
		this.lnaGain = options.lnaGain;
		this.vgaGain = options.vgaGain;

		// Rust Wasm側のReceiverインスタンスを作成
		this.receiver = new this.wasmBindings.Receiver(
			options.sampleRate,
			options.centerFreq,
			options.targetFreq,
			options.demodMode,
			options.outputSampleRate,
			options.fftSize,
			options.fftVisibleStartBin,
			options.fftVisibleBins,
			options.ifMinHz,
			options.ifMaxHz,
			options.dcCancelEnabled
		);

		// デバイス側にサンプリングレートおよび周波数を設定
		// stop/start の繰り返しでRF設定が揮発するケースを避けるため、毎回再適用する。
		await this.device.stopRx();
		await this.device.setAmpEnable(this.ampEnabled);
		await this.device.setAntennaEnable(this.antennaEnabled);
		await this.device.setLnaGain(this.lnaGain);
		await this.device.setVgaGain(this.vgaGain);
		await this.device.setSampleRateManual(options.sampleRate, 1);
		await this.device.setFreq(options.centerFreq);

		const mode = options.demodMode.toUpperCase();
		const demodRate = mode === "FM" ? 200_000 : 50_000;
		const coarseFactor = Math.max(1, Math.round(options.sampleRate / 1_000_000));
		const coarseRate = options.sampleRate / coarseFactor;
		const demodFactor = Math.max(1, Math.round(coarseRate / demodRate));
		const audioChannels = Math.max(1, Math.min(2, this.receiver.audio_output_channels()));
		const iqSamplesPerBlock = HackRF.TRANSFER_BUFFER_SIZE / 2;
		const demodSamplesPerBlock = Math.ceil(iqSamplesPerBlock / coarseFactor / demodFactor);
		const audioCapacity = Math.max(
			1024,
			Math.ceil(demodSamplesPerBlock * (options.outputSampleRate / demodRate) * 2 * audioChannels)
		);
		const fftCapacity = options.fftVisibleBins;
		await this.receiver.alloc_io_buffers(
			HackRF.TRANSFER_BUFFER_SIZE,
			audioCapacity,
			fftCapacity
		);

		if (!this.wasmModule || !this.wasmModule.memory) {
			throw new Error("wasm memory is not initialized");
		}
		const wasmMemory: WebAssembly.Memory = this.wasmModule.memory;
		const iqPtr = this.receiver.iq_input_ptr();
		const audioPtr = this.receiver.audio_output_ptr();
		const fftPtr = this.receiver.fft_output_ptr();
		const iqCapacity = this.receiver.iq_input_capacity();
		const audioOutCapacity = this.receiver.audio_output_capacity();
		const fftOutCapacity = this.receiver.fft_output_capacity();

		if (iqCapacity < HackRF.TRANSFER_BUFFER_SIZE) {
			throw new Error("iq capacity is smaller than transfer block size");
		}
		if (audioOutCapacity < audioCapacity || fftOutCapacity < fftCapacity) {
			throw new Error("allocated io capacity is smaller than requested");
		}

		let memoryBuffer = wasmMemory.buffer;
		let iqWriteView = new Uint8Array(memoryBuffer, iqPtr, iqCapacity);
		let audioReadView = new Float32Array(memoryBuffer, audioPtr, audioOutCapacity);
		let fftReadView = new Float32Array(memoryBuffer, fftPtr, fftCapacity);

		const ensureViews = () => {
			if (memoryBuffer === wasmMemory.buffer) return;
			memoryBuffer = wasmMemory.buffer;
			iqWriteView = new Uint8Array(memoryBuffer, iqPtr, iqCapacity);
			audioReadView = new Float32Array(memoryBuffer, audioPtr, audioOutCapacity);
			fftReadView = new Float32Array(memoryBuffer, fftPtr, fftCapacity);
		};

		const fftScratch = new Float32Array(fftCapacity);

		let perfStarted = false;
		let perfWindowStart = 0;
		let perfTotalStart = 0;
		let lastBlockAt = 0;
		let blockCount = 0;
		let droppedIqBlocksCount = 0;
		let blockIntervalMsPeak = 0;
		let dspProcessMsPeak = 0;
		let audioFramesOutTotal = 0;

		const readNum = (value: unknown): number | undefined => {
			return typeof value === "number" && Number.isFinite(value) ? value : undefined;
		};
		const readBool = (value: unknown): boolean | undefined => {
			return typeof value === "boolean" ? value : undefined;
		};
		const readStatNum = (src: Record<string, unknown>, ...keys: string[]): number => {
			for (const k of keys) {
				const v = readNum(src[k]);
				if (v !== undefined) return v;
			}
			return 0;
		};
		const readStatBool = (src: Record<string, unknown>, ...keys: string[]): boolean => {
			for (const k of keys) {
				const v = readBool(src[k]);
				if (v !== undefined) return v;
			}
			return false;
		};

		const snapshotPerf = (now: number): PerfStats | undefined => {
			if (!perfStarted) return undefined;
			const windowMs = now - perfWindowStart;
			if (windowMs < 1000 || blockCount === 0) return undefined;

			const totalSec = Math.max(0.000001, (now - perfTotalStart) / 1000);
			const demodStatsRaw = this.receiver?.get_stats?.();
			const demodStats =
				demodStatsRaw && typeof demodStatsRaw === "object"
					? (demodStatsRaw as Record<string, unknown>)
					: {};
			const stats: PerfStats = {
				droppedIqBlocksCount,
				blockIntervalMsPeak,
				dspProcessMsPeak,
				audioOutHzLong: audioFramesOutTotal / totalSec,
				pilotLevel: readStatNum(demodStats, "pilotLevel", "pilot_level"),
				stereoBlend: readStatNum(demodStats, "stereoBlend", "stereo_blend"),
				stereoLocked: readStatBool(demodStats, "stereoLocked", "stereo_locked"),
				monoFallbackCount: readStatNum(demodStats, "monoFallbackCount", "mono_fallback_count"),
			};

			perfWindowStart = now;
			blockCount = 0;
			return stats;
		};

		await this.device.startRx((data: Uint8Array) => {
			if (!this.receiver) return;
			const now = performance.now();
			if (!perfStarted) {
				perfStarted = true;
				perfWindowStart = now;
				perfTotalStart = now;
				lastBlockAt = now;
			}
			if (blockCount > 0) {
				const blockIntervalMs = now - lastBlockAt;
				if (blockIntervalMs > blockIntervalMsPeak) {
					blockIntervalMsPeak = blockIntervalMs;
				}
			}
			lastBlockAt = now;
			blockCount += 1;

			if (data.byteLength > iqCapacity) {
				droppedIqBlocksCount += 1;
				return;
			}

			// WASM I/O バッファへ転送後、長さだけ渡して処理する。
			ensureViews();
			iqWriteView.set(data.subarray(0, data.byteLength), 0);
			const processStart = performance.now();
			let audioLen = 0;
			try {
				audioLen = this.receiver.process_iq_len(data.byteLength);
			} catch (_e) {
				droppedIqBlocksCount += 1;
				return;
			}
			// process中にWasmメモリが再配置される場合があるため再取得する
			ensureViews();
			const processMs = performance.now() - processStart;
			if (processMs > dspProcessMsPeak) {
				dspProcessMsPeak = processMs;
			}
			if (audioLen >= 0) {
				if (audioLen > 0) {
					if (this.audioPort) {
						const audioChunk = new Float32Array(audioLen);
						audioChunk.set(audioReadView.subarray(0, audioLen));
						this.audioPort.postMessage(
							{ type: "push", channels: audioChannels, data: audioChunk },
							[audioChunk.buffer]
						);
					}
				}
				fftScratch.set(fftReadView);
				audioFramesOutTotal += Math.floor(audioLen / audioChannels);
				const perf = snapshotPerf(performance.now());
				onData(fftScratch, perf);
			}
		});
	}

	async stopRx() {
		if (this.device) {
			await this.device.stopRx();
		}
		if (this.audioPort) {
			try {
				this.audioPort.postMessage({ type: "reset" });
			} catch (_e) {
				// no-op
			}
		}
		if (this.receiver) {
			this.receiver.free_io_buffers();
			this.receiver.free();
			this.receiver = undefined;
		}
	}

	async setTargetFreq(centerFreq: number, targetFreq: number) {
		if (this.receiver) {
			this.receiver.set_target_freq(centerFreq, targetFreq);
		}
	}

	async setIfBand(minHz: number, maxHz: number) {
		if (this.receiver) {
			this.receiver.set_if_band(minHz, maxHz);
		}
	}

	async setDcCancelEnabled(enabled: boolean) {
		if (this.receiver) {
			this.receiver.set_dc_cancel_enabled(enabled);
		}
	}
}

expose(RadioBackend);
