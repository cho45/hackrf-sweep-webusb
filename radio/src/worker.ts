import { expose } from "comlink";
import init, { Receiver } from "../hackrf-dsp/pkg/hackrf_dsp";
import { HackRF } from "./hackrf";

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
		onData: (audioOut: Float32Array, fftOut: Float32Array) => void
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

		await this.device.startRx((data: Uint8Array) => {
			if (!this.receiver) return;

			// Uint8Array は uint8 (0~255) だが HackRF の IQ データは i8 (-128~127) のため Int8Array にキャスト
			const iqData = new Int8Array(data.buffer, data.byteOffset, data.byteLength);

			// WASM に IQ データ配列を渡し、復調処理とFFTを実行
			const out = this.receiver.process(iqData);
			if (out && out.length === 2) {
				const audioOut = (out[0] as unknown) as Float32Array;
				const fftOut = (out[1] as unknown) as Float32Array;
				onData(audioOut, fftOut);
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
