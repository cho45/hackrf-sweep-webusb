import { expose } from "comlink";
import initBase, { Receiver as ReceiverBase } from "../hackrf-dsp/pkg/hackrf_dsp";
import initSimd, { Receiver as ReceiverSimd } from "../hackrf-dsp/pkg-simd/hackrf_dsp";
import { HackRF } from "./hackrf";

type PerfStats = {
	// USB入力欠落が起きていないか（受信パス健全性）
	droppedIqBlocksCount: number;
	// USB/スケジューリング由来の停止スパイク検知
	blockIntervalMsPeak: number;
	// 直近ブロックのDSP処理時間
	dspProcessMsLast: number;
	// DSP処理が詰まり要因になっていないか
	dspProcessMsPeak: number;
	// 長期の供給不足判定（短窓の揺れは見ない）
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
	set_fft_view: (startBin: number, visibleBins: number) => void;
	free: () => void;
	set_target_freq: (centerFreq: number, targetFreq: number) => void;
	set_if_band: (minHz: number, maxHz: number) => void;
	set_dc_cancel_enabled: (enabled: boolean) => void;
	set_fm_stereo_enabled: (enabled: boolean) => void;
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

const AGC_ACCEPT_MIN = 70;
const AGC_ACCEPT_MAX = 110;
const AGC_HARD_HIGH = 120;
const AGC_HARD_LOW = 50;
const AGC_MAX_ITERATIONS = 4;
const AGC_WAIT_STATS_TIMEOUT_MS = 3000;
const AGC_SETTLE_MS = 300;
const AGC_VGA_SOFT_MAX = 40;
const AGC_SNR_IMPROVE_MIN_DB = 0.6;
const FFT_SIGNAL_NEIGHBOR_BINS = 1;

const toAbortError = () => new DOMException("auto gain aborted", "AbortError");

const throwIfAborted = (signal?: AbortSignal) => {
	if (signal?.aborted) throw toAbortError();
};

const waitMs = (ms: number, signal?: AbortSignal) =>
	new Promise<void>((resolve, reject) => {
		if (signal?.aborted) {
			reject(toAbortError());
			return;
		}
		const timer = setTimeout(() => {
			signal?.removeEventListener("abort", onAbort);
			resolve();
		}, ms);
		const onAbort = () => {
			clearTimeout(timer);
			reject(toAbortError());
		};
		signal?.addEventListener("abort", onAbort, { once: true });
	});

const clamp = (v: number, min: number, max: number) => Math.min(max, Math.max(min, v));

const computeFftQuality = (
	fftBins: Float32Array,
	targetBin: number
): { targetDb: number; noiseFloorDb: number; snrDb: number } => {
	if (fftBins.length === 0) {
		return { targetDb: 0, noiseFloorDb: 0, snrDb: 0 };
	}
	const t = clamp(Math.round(targetBin), 0, fftBins.length - 1);
	let targetDb = -120;
	const signalStart = clamp(t - FFT_SIGNAL_NEIGHBOR_BINS, 0, fftBins.length - 1);
	const signalEnd = clamp(t + FFT_SIGNAL_NEIGHBOR_BINS, 0, fftBins.length - 1);
	for (let i = signalStart; i <= signalEnd; i += 1) {
		const v = fftBins[i]!;
		if (v > targetDb) targetDb = v;
	}

	const sorted = Array.from(fftBins).sort((a, b) => a - b);
	const mid = Math.floor(sorted.length / 2);
	const noiseFloorDb = sorted.length % 2 === 0
		? ((sorted[mid - 1] ?? -120) + (sorted[mid] ?? -120)) * 0.5
		: (sorted[mid] ?? -120);
	const snrDb = targetDb - noiseFloorDb;
	return { targetDb, noiseFloorDb, snrDb };
};

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
	private fmStereoEnabled = true;
	private fftVisibleBins = 0;
	private sampleRate = 0;
	private centerFreq = 0;
	private targetFreq = 0;
	private fftSize = 0;
	private fftVisibleStartBin = 0;
	private targetVisibleBin = 0;
	private latestPerfStats?: PerfStats;
	private latestPerfSeq = 0;
	private autoGainPromise: Promise<AutoGainResult> | null = null;
	private autoGainAbortController: AbortController | null = null;

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

	private normalizedVgaGain(val: number): number {
		const clamped = Math.max(0, Math.min(62, Math.round(val)));
		return clamped & ~0x01;
	}

	private normalizedLnaGain(val: number): number {
		const clamped = Math.max(0, Math.min(40, Math.round(val)));
		return clamped & ~0x07;
	}

	private updateTargetVisibleBin() {
		if (this.sampleRate <= 0 || this.fftSize <= 0 || this.fftVisibleBins <= 0) {
			this.targetVisibleBin = 0;
			return;
		}
		const rel = (this.targetFreq - this.centerFreq) / this.sampleRate;
		const absBin = Math.round((rel + 0.5) * this.fftSize);
		this.targetVisibleBin = clamp(absBin - this.fftVisibleStartBin, 0, this.fftVisibleBins - 1);
	}

	private async waitForNextPerfStats(timeoutMs: number, signal?: AbortSignal): Promise<PerfStats> {
		const startSeq = this.latestPerfSeq;
		const startedAt = performance.now();
		while (performance.now() - startedAt < timeoutMs) {
			throwIfAborted(signal);
			if (this.latestPerfStats && this.latestPerfSeq > startSeq) {
				return this.latestPerfStats;
			}
			await waitMs(50, signal);
		}
		throw new Error("timed out waiting for stats");
	}

	private async applyStepForPeak(adcPeak: number): Promise<string | null> {
		if (adcPeak >= AGC_HARD_HIGH) {
			if (this.ampEnabled) {
				await this.setAmpEnable(false);
				return "amp_off";
			}
			if (this.lnaGain > 0) {
				await this.setLnaGain(this.normalizedLnaGain(this.lnaGain - 8));
				return "lna_down_8";
			}
			if (this.vgaGain > 0) {
				const down = this.vgaGain >= 4 ? 4 : 2;
				await this.setVgaGain(this.normalizedVgaGain(this.vgaGain - down));
				return down === 4 ? "vga_down_4" : "vga_down_2";
			}
			return null;
		}
		if (adcPeak <= AGC_HARD_LOW) {
			if (this.vgaGain < AGC_VGA_SOFT_MAX) {
				const up = this.vgaGain <= AGC_VGA_SOFT_MAX - 4 ? 4 : 2;
				await this.setVgaGain(this.normalizedVgaGain(this.vgaGain + up));
				return up === 4 ? "vga_up_4" : "vga_up_2";
			}
			if (this.lnaGain < 40) {
				await this.setLnaGain(this.normalizedLnaGain(this.lnaGain + 8));
				return "lna_up_8";
			}
			if (this.vgaGain < 62) {
				const up = this.vgaGain <= 58 ? 4 : 2;
				await this.setVgaGain(this.normalizedVgaGain(this.vgaGain + up));
				return up === 4 ? "vga_up_4" : "vga_up_2";
			}
			if (!this.ampEnabled) {
				await this.setAmpEnable(true);
				return "amp_on";
			}
			return null;
		}
		if (adcPeak < AGC_ACCEPT_MIN) {
			if (this.vgaGain < AGC_VGA_SOFT_MAX) {
				await this.setVgaGain(this.normalizedVgaGain(this.vgaGain + 2));
				return "vga_up_2";
			}
			if (this.lnaGain < 40) {
				await this.setLnaGain(this.normalizedLnaGain(this.lnaGain + 8));
				return "lna_up_8";
			}
			if (this.vgaGain < 62) {
				await this.setVgaGain(this.normalizedVgaGain(this.vgaGain + 2));
				return "vga_up_2";
			}
			if (!this.ampEnabled) {
				await this.setAmpEnable(true);
				return "amp_on";
			}
			return null;
		}
		if (adcPeak > AGC_ACCEPT_MAX) {
			if (this.vgaGain > 0) {
				await this.setVgaGain(this.normalizedVgaGain(this.vgaGain - 2));
				return "vga_down_2";
			}
			if (this.lnaGain > 0) {
				await this.setLnaGain(this.normalizedLnaGain(this.lnaGain - 8));
				return "lna_down_8";
			}
			if (this.ampEnabled) {
				await this.setAmpEnable(false);
				return "amp_off";
			}
			return null;
		}
		return null;
	}

	autoSetGainOnce(): Promise<AutoGainResult> {
		if (!this.device || !this.receiver) {
			throw new Error("receiver is not running");
		}
		if (this.autoGainPromise) {
			return this.autoGainPromise;
		}

		this.autoGainAbortController = new AbortController();
		const signal = this.autoGainAbortController.signal;

		const run = async (): Promise<AutoGainResult> => {
			throwIfAborted(signal);
			const initialStats = await this.waitForNextPerfStats(AGC_WAIT_STATS_TIMEOUT_MS, signal);
			let currentPeak = initialStats.adcPeak;
			let currentSnr = Number.isFinite(initialStats.fftSnrDb) ? initialStats.fftSnrDb : 0;
			const initialPeak = currentPeak;
			const appliedSteps: string[] = [];

			for (let i = 0; i < AGC_MAX_ITERATIONS; i += 1) {
				throwIfAborted(signal);
				if (currentPeak >= AGC_ACCEPT_MIN && currentPeak <= AGC_ACCEPT_MAX) {
					break;
				}
				const applied = await this.applyStepForPeak(currentPeak);
				if (!applied) {
					break;
				}
				appliedSteps.push(applied);
				await waitMs(AGC_SETTLE_MS, signal);
				let nextStats = await this.waitForNextPerfStats(AGC_WAIT_STATS_TIMEOUT_MS, signal);
				let nextPeak = nextStats.adcPeak;
				let nextSnr = Number.isFinite(nextStats.fftSnrDb) ? nextStats.fftSnrDb : 0;

				const vgaRaised = applied === "vga_up_2" || applied === "vga_up_4";
				const vgaIneffective =
					vgaRaised &&
					nextPeak <= AGC_ACCEPT_MIN &&
					nextSnr < currentSnr + AGC_SNR_IMPROVE_MIN_DB &&
					this.lnaGain < 40;
				if (vgaIneffective) {
					if (applied === "vga_up_4") {
						await this.setVgaGain(this.normalizedVgaGain(this.vgaGain - 4));
						appliedSteps.push("vga_down_4(rollback)");
					} else {
						await this.setVgaGain(this.normalizedVgaGain(this.vgaGain - 2));
						appliedSteps.push("vga_down_2(rollback)");
					}
					await waitMs(AGC_SETTLE_MS, signal);
					await this.setLnaGain(this.normalizedLnaGain(this.lnaGain + 8));
					appliedSteps.push("lna_up_8");
					await waitMs(AGC_SETTLE_MS, signal);
					nextStats = await this.waitForNextPerfStats(AGC_WAIT_STATS_TIMEOUT_MS, signal);
					nextPeak = nextStats.adcPeak;
					nextSnr = Number.isFinite(nextStats.fftSnrDb) ? nextStats.fftSnrDb : 0;
				}

				currentPeak = nextPeak;
				currentSnr = nextSnr;
			}

			const settled = currentPeak >= AGC_ACCEPT_MIN && currentPeak <= AGC_ACCEPT_MAX;
			return {
				initialPeak,
				finalPeak: currentPeak,
				iterations: appliedSteps.length,
				appliedSteps,
				ampEnabled: this.ampEnabled,
				lnaGain: this.lnaGain,
				vgaGain: this.vgaGain,
				settled,
			};
		};

		let promise: Promise<AutoGainResult>;
		promise = run().finally(() => {
			if (this.autoGainPromise === promise) {
				this.autoGainPromise = null;
				this.autoGainAbortController = null;
			}
		});
		this.autoGainPromise = promise;
		return promise;
	}

	cancelAutoSetGain() {
		this.autoGainAbortController?.abort();
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
			fmStereoEnabled: boolean;
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
		this.fmStereoEnabled = options.fmStereoEnabled;
		this.sampleRate = options.sampleRate;
		this.centerFreq = options.centerFreq;
		this.targetFreq = options.targetFreq;
		this.fftSize = options.fftSize;
		this.fftVisibleStartBin = options.fftVisibleStartBin;
		this.fftVisibleBins = options.fftVisibleBins;
		this.updateTargetVisibleBin();

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
		this.receiver.set_fm_stereo_enabled(this.fmStereoEnabled);

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
		const fftCapacity = options.fftSize;
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
		let fftReadView = new Float32Array(memoryBuffer, fftPtr, fftOutCapacity);

		const ensureViews = () => {
			if (memoryBuffer === wasmMemory.buffer) return;
			memoryBuffer = wasmMemory.buffer;
			iqWriteView = new Uint8Array(memoryBuffer, iqPtr, iqCapacity);
			audioReadView = new Float32Array(memoryBuffer, audioPtr, audioOutCapacity);
				fftReadView = new Float32Array(memoryBuffer, fftPtr, fftOutCapacity);
			};

			let fftScratch = new Float32Array(this.fftVisibleBins);
			this.latestPerfStats = undefined;

		let perfStarted = false;
		let perfWindowStart = 0;
		let perfTotalStart = 0;
		let lastBlockAt = 0;
		let blockCount = 0;
		let droppedIqBlocksCount = 0;
		let blockIntervalMsPeak = 0;
		let dspProcessMsLast = 0;
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

			const snapshotPerf = (now: number, fftFrame?: Float32Array): PerfStats | undefined => {
				if (!perfStarted) return undefined;
				const windowMs = now - perfWindowStart;
				if (windowMs < 1000 || blockCount === 0) return undefined;

			const totalSec = Math.max(0.000001, (now - perfTotalStart) / 1000);
			const demodStatsRaw = this.receiver?.get_stats?.();
			const demodStats =
				demodStatsRaw && typeof demodStatsRaw === "object"
					? (demodStatsRaw as Record<string, unknown>)
					: {};
				const fftQuality = fftFrame
					? computeFftQuality(fftFrame, this.targetVisibleBin)
					: { targetDb: 0, noiseFloorDb: 0, snrDb: 0 };
				const stats: PerfStats = {
					droppedIqBlocksCount,
					blockIntervalMsPeak,
					dspProcessMsLast,
					dspProcessMsPeak,
					audioOutHzLong: audioFramesOutTotal / totalSec,
					fmStereoPilotLevel: readStatNum(
						demodStats,
						"fmStereoPilotLevel",
						"fm_stereo_pilot_level",
						"pilotLevel",
						"pilot_level"
					),
					fmStereoBlend: readStatNum(
						demodStats,
						"fmStereoBlend",
						"fm_stereo_blend",
						"stereoBlend",
						"stereo_blend"
					),
					fmStereoLocked: readStatBool(
						demodStats,
						"fmStereoLocked",
						"fm_stereo_locked",
						"stereoLocked",
						"stereo_locked"
					),
					fmStereoMonoFallbackCount: readStatNum(
						demodStats,
						"fmStereoMonoFallbackCount",
						"fm_stereo_mono_fallback_count",
						"monoFallbackCount",
						"mono_fallback_count"
					),
					fmStereoPllLocked: readStatBool(
						demodStats,
						"fmStereoPllLocked",
						"fm_stereo_pll_locked",
						"pllLocked",
						"pll_locked"
					),
					adcPeak: readStatNum(demodStats, "adcPeak", "adc_peak"),
					fftTargetDb: fftQuality.targetDb,
					fftNoiseFloorDb: fftQuality.noiseFloorDb,
					fftSnrDb: fftQuality.snrDb,
				};
					this.latestPerfStats = stats;
					this.latestPerfSeq += 1;

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
			dspProcessMsLast = processMs;
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
					const visibleBins = Math.max(
						1,
						Math.min(this.fftVisibleBins || fftOutCapacity, fftOutCapacity)
					);
					if (fftScratch.length !== visibleBins) {
						fftScratch = new Float32Array(visibleBins);
					}
					fftScratch.set(fftReadView.subarray(0, visibleBins));
					audioFramesOutTotal += Math.floor(audioLen / audioChannels);
						const perf = snapshotPerf(performance.now(), fftScratch);
						onData(fftScratch, perf);
				}
			});
		}

	async stopRx() {
		this.cancelAutoSetGain();
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
		this.fftVisibleBins = 0;
		this.sampleRate = 0;
		this.fftSize = 0;
		this.fftVisibleStartBin = 0;
		this.targetVisibleBin = 0;
		this.latestPerfStats = undefined;
	}

	async setTargetFreq(centerFreq: number, targetFreq: number) {
		this.centerFreq = centerFreq;
		this.targetFreq = targetFreq;
		this.updateTargetVisibleBin();
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

	async setFmStereoEnabled(enabled: boolean) {
		this.fmStereoEnabled = enabled;
		if (this.receiver) {
			this.receiver.set_fm_stereo_enabled(enabled);
		}
	}

	async setFftView(startBin: number, visibleBins: number) {
		this.fftVisibleStartBin = startBin;
		this.fftVisibleBins = visibleBins;
		this.updateTargetVisibleBin();
		if (this.receiver) {
			this.receiver.set_fft_view(startBin, visibleBins);
		}
	}
}

expose(RadioBackend);
