import { expose } from "comlink";
import init, { Receiver } from "../hackrf-dsp/pkg/hackrf_dsp";
import { HackRF } from "./hackrf";

type PerfStats = {
	windowMs: number;
	blocks: number;
	blocksPerSec: number;
	iqBytesPerSec: number;
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
		onData: (audioOut: Float32Array, fftOut: Float32Array, perf?: PerfStats) => void
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
		const audioScratchCapacity = Math.max(
			1024,
			Math.ceil(demodSamplesPerBlock * (options.outputSampleRate / demodRate) * 2)
		);
		const audioScratch = new Float32Array(audioScratchCapacity);
		const fftScratch = new Float32Array(options.fftVisibleBins);

		let perfWindowStart = performance.now();
		let lastBlockAt = performance.now();
		let blockCount = 0;
		let iqBytes = 0;
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

			// Uint8Array は uint8 (0~255) だが HackRF の IQ データは i8 (-128~127) のため Int8Array にキャスト
			const iqData = new Int8Array(data.buffer, data.byteOffset, data.byteLength);

			// WASM に IQ データ配列を渡し、復調処理とFFTを実行
			const processStart = performance.now();
			const audioLen = this.receiver.process_into(iqData, audioScratch, fftScratch);
			const processMs = performance.now() - processStart;
			processMsSum += processMs;
			if (processMs > processMsMax) {
				processMsMax = processMs;
			}
			if (audioLen >= 0) {
				const audioOut = audioScratch.subarray(0, audioLen);
				audioSamplesOut += audioLen;
				const callbackStart = performance.now();
				const perf = snapshotPerf(callbackStart);
				onData(audioOut, fftScratch, perf);
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
