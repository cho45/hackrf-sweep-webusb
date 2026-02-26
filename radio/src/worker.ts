import { expose } from "comlink";
import init, { Receiver } from "../hackrf-dsp/pkg/hackrf_dsp";
import { HackRF } from "./hackrf";

type PerfStats = {
	windowMs: number;
	blocks: number;
	blocksPerSec: number;
	iqBytesPerSec: number;
	droppedIqBlocks: number;
	droppedIqBlocksPerSec: number;
	blockIntervalMsAvg: number;
	blockIntervalMsMax: number;
	dspProcessMsAvg: number;
	dspProcessMsMax: number;
	callbackMsAvg: number;
	callbackMsMax: number;
	audioSamplesPerSec: number;
};

export class RadioBackend {
	device?: HackRF;
	receiver?: Receiver;
	wasmModule?: any;
	private ampEnabled = false;
	private antennaEnabled = false;
	private lnaGain = 16;
	private vgaGain = 16;

	async init() {
		this.wasmModule = await init();
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
			fftUseProcessed: boolean;
			ampEnabled: boolean;
			antennaEnabled: boolean;
			lnaGain: number;
			vgaGain: number;
		},
		onData: (audioOut: Float32Array, audioLen: number, fftOut: Float32Array, perf?: PerfStats) => void
	) {
		if (!this.device) throw new Error("device not opened");
		this.ampEnabled = options.ampEnabled;
		this.antennaEnabled = options.antennaEnabled;
		this.lnaGain = options.lnaGain;
		this.vgaGain = options.vgaGain;

		// Rust Wasm側のReceiverインスタンスを作成
		this.receiver = new Receiver(
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
			options.dcCancelEnabled,
			options.fftUseProcessed
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
		const iqSamplesPerBlock = HackRF.TRANSFER_BUFFER_SIZE / 2;
		const demodSamplesPerBlock = Math.ceil(iqSamplesPerBlock / coarseFactor / demodFactor);
		const audioCapacity = Math.max(
			1024,
			Math.ceil(demodSamplesPerBlock * (options.outputSampleRate / demodRate) * 2)
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

		const audioScratch = new Float32Array(audioOutCapacity);
		const fftScratch = new Float32Array(fftCapacity);

		let perfWindowStart = performance.now();
		let lastBlockAt = performance.now();
		let blockCount = 0;
		let iqBytes = 0;
		let droppedIqBlocks = 0;
		let blockIntervalMsSum = 0;
		let blockIntervalMsMax = 0;
		let processMsSum = 0;
		let processMsMax = 0;
		let callbackMsSum = 0;
		let callbackMsMax = 0;
		let audioSamplesOut = 0;

		const snapshotPerf = (now: number): PerfStats | undefined => {
			const windowMs = now - perfWindowStart;
			if (windowMs < 1000 || blockCount === 0) return undefined;

			const windowSec = windowMs / 1000;
			const stats: PerfStats = {
				windowMs,
				blocks: blockCount,
				blocksPerSec: blockCount / windowSec,
				iqBytesPerSec: iqBytes / windowSec,
				droppedIqBlocks,
				droppedIqBlocksPerSec: droppedIqBlocks / windowSec,
				blockIntervalMsAvg: blockIntervalMsSum / blockCount,
				blockIntervalMsMax,
				dspProcessMsAvg: processMsSum / blockCount,
				dspProcessMsMax: processMsMax,
				callbackMsAvg: callbackMsSum / blockCount,
				callbackMsMax: callbackMsMax,
				audioSamplesPerSec: audioSamplesOut / windowSec,
			};

			perfWindowStart = now;
			blockCount = 0;
			iqBytes = 0;
			droppedIqBlocks = 0;
			blockIntervalMsSum = 0;
			blockIntervalMsMax = 0;
			processMsSum = 0;
			processMsMax = 0;
			callbackMsSum = 0;
			callbackMsMax = 0;
			audioSamplesOut = 0;
			return stats;
		};

		await this.device.startRx((data: Uint8Array) => {
			if (!this.receiver) return;
			const now = performance.now();
			const blockIntervalMs = now - lastBlockAt;
			lastBlockAt = now;
			blockCount += 1;
			iqBytes += data.byteLength;
			blockIntervalMsSum += blockIntervalMs;
			if (blockIntervalMs > blockIntervalMsMax) {
				blockIntervalMsMax = blockIntervalMs;
			}

			if (data.byteLength > iqCapacity) {
				droppedIqBlocks += 1;
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
				droppedIqBlocks += 1;
				return;
			}
			const processMs = performance.now() - processStart;
			processMsSum += processMs;
			if (processMs > processMsMax) {
				processMsMax = processMs;
			}
			if (audioLen >= 0) {
				if (audioLen > 0) {
					audioScratch.set(audioReadView.subarray(0, audioLen), 0);
				}
				fftScratch.set(fftReadView);
				audioSamplesOut += audioLen;
				const callbackStart = performance.now();
				const perf = snapshotPerf(callbackStart);
				onData(audioScratch, audioLen, fftScratch, perf);
				const callbackMs = performance.now() - callbackStart;
				callbackMsSum += callbackMs;
				if (callbackMs > callbackMsMax) {
					callbackMsMax = callbackMs;
				}
			}
		});
	}

	async stopRx() {
		if (this.device) {
			await this.device.stopRx();
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

	async setFftUseProcessed(enabled: boolean) {
		if (this.receiver) {
			this.receiver.set_fft_use_processed(enabled);
		}
	}
}

expose(RadioBackend);
