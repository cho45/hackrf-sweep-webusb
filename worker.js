
//import * as Comlink from "./node_modules/comlink/dist/esm/comlink.mjs";
importScripts("./node_modules/comlink/dist/umd/comlink.js");
importScripts("./hackrf.js");

const lib = {};
(() => {
	importScripts("./hackrf-web/no-modules/hackrf_web.js");
	lib.wasm_bindgen = self.wasm_bindgen;
	console.log(lib);
})();

class Worker {
	constructor() {
	}

	async init() {
		console.log('init worker');
		await lib.wasm_bindgen("./hackrf-web/no-modules/hackrf_web_bg.wasm");
	}

	async open(opts) {

		const devices = await navigator.usb.getDevices();
		const device = !opts ? devices[0] : devices.find( d => {
			if (opts.vendorId) {
				if (d.vendorId !== opts.vendorId) {
					return false;
				}
			}
			if (opts.productId) {
				if (d.productId !== opts.productId) {
					return false;
				}
			} 
			if (opts.serialNumber) {
				if (d.serialNumber !== opts.serialNumber) {
					return false;
				}
			}
			return true;
		});
		if (!device) {
			return false;
		}
		console.log(device);
		this.hackrf = new HackRF();
		await this.hackrf.open(device);
		return true;
	}

	async info() {
		const { hackrf } = this;
		const boardId = await hackrf.readBoardId();
		const versionString = await hackrf.readVersionString();
		const apiVersion = await hackrf.readApiVersion();
		const { partId, serialNo } = await hackrf.readPartIdSerialNo();

		console.log(`Serial Number: ${serialNo.map( (i) => (i + 0x100000000).toString(16).slice(1) ).join('')}`)
		console.log(`Board ID Number: ${boardId} (${HackRF.BOARD_ID_NAME[boardId]})`);
		console.log(`Firmware Version: ${versionString} (API:${apiVersion[0]}.${apiVersion[1]}${apiVersion[2]})`);
		console.log(`Part ID Number: ${partId.map( (i) => (i + 0x100000000).toString(16).slice(1) ).join(' ')}`)
		return {boardId, versionString, apiVersion, partId, serialNo };
	}

	async start(opts, callback) {
		const { hackrf } = this;

		const { FFT_SIZE, SAMPLE_RATE, lowFreq, highFreq, bandwidth, freqBinCount } = opts;
		console.log({lowFreq, highFreq, bandwidth, freqBinCount});

		await hackrf.setSampleRateManual(SAMPLE_RATE, 1);
		await hackrf.setBasebandFilterBandwidth(15e6);

		const windowFunction = (x) => {
			// blackman window
			const alpha = 0.16;
			const a0 = (1.0 - alpha) / 2.0;
			const a1 = 1.0 / 2.0;
			const a2 = alpha / 2.0;
			return  a0 - a1 * Math.cos(2 * Math.PI * x) + a2 * Math.cos(4 * Math.PI * x);
		};

		const window = new Float32Array(FFT_SIZE);
		for (let i = 0; i < FFT_SIZE; i++) {
			window[i] = windowFunction(i / FFT_SIZE);
		}

		const fft = new lib.wasm_bindgen.FFT(FFT_SIZE, window);
		const line   = new Float32Array(freqBinCount);
		const output = new Float32Array(FFT_SIZE);
		await hackrf.startRx((data) => {
			let o = 0;
			for (let n = 0, len = 16; n < len; n++) {
				// console.log(o % HackRF.BYTES_PER_BLOCK, n, data[o+0], data[o+1]);
				if (!(data[o+0] === 0x7F && data[o+1] === 0x7F)) {
					console.log('invalid header', n, data[o+0], data[o+1]);
					o += HackRF.BYTES_PER_BLOCK;
					continue;
				}

				// this is sweep mode
				// JavaScript does not support 64bit, and all bit operations treat number as 32bit.
				// but double can retain 53bit integer (Number.MAX_SAFE_INTEGER) and frequency never exceeds Number.MAX_SAFE_INTEGER
				// so we can calculate with generic floating point math operation.
				const freqH = (
					(data[o+9] << 24) |
					(data[o+8] << 16) |
					(data[o+7] <<  8) |
					(data[o+6] <<  0) )>>>0;
				const freqL = (
					(data[o+5] << 24) |
					(data[o+4] << 16) |
					(data[o+3] <<  8) |
					(data[o+2] <<  0) )>>>0;
				const frequency = 2**32*freqH + freqL;

				const freqM = frequency / 1e6;

				if (freqM < lowFreq) {
					console.log(freqM, 'ignored');
					o += HackRF.BYTES_PER_BLOCK;
					continue;
				} else
				if (freqM > highFreq) {
					console.log(freqM, 'ignored');
					o += HackRF.BYTES_PER_BLOCK;
					continue
				} else
				if (freqM === lowFreq) {
					callback(line);
					line.fill(0);
				}

				o += HackRF.BYTES_PER_BLOCK - (FFT_SIZE * 2);
				const target = data.subarray(o, o + FFT_SIZE * 2);
				fft.fft(target, output);
				o += FFT_SIZE * 2;

				//*
				let pos = Math.floor((freqM - lowFreq) / bandwidth * freqBinCount);
				const low = output.subarray(Math.floor(FFT_SIZE/8*1), Math.ceil(FFT_SIZE/8*3));
				if (pos < line.length) line.set(low.subarray(0, (line.length - pos)), pos);
				const pos2 = pos + FFT_SIZE/2;
				const high = output.subarray(Math.floor(FFT_SIZE/8*5), Math.ceil(FFT_SIZE/8*7));
				if (pos2 < line.length) line.set(high.subarray(0, (line.length - pos2)), pos2);
				// console.log({freqM, pos, pos2}, output.length, line.length);
				//*/
			}
		});

		console.log('initSweep', [
			[lowFreq, highFreq],
			HackRF.SAMPLES_PER_BLOCK * 2 /* I + Q */,
			SAMPLE_RATE,
			SAMPLE_RATE / 8 * 3,
			HackRF.SWEEP_STYLE_INTERLEAVED
		]);
		await hackrf.initSweep(
			[lowFreq, highFreq],
			HackRF.SAMPLES_PER_BLOCK * 2 /* I + Q */,
			SAMPLE_RATE,
			SAMPLE_RATE / 8 * 3,
			HackRF.SWEEP_STYLE_INTERLEAVED
		);
	}

	async setSampleRateManual(freq, divider) {
		await this.hackrf.setSampleRateManual(freq, devider);
	}

	async setBasebandFilterBandwidth(bandwidthHz) {
		await this.hackrf.setBasebandFilterBandwidth(bandwidthHz);
	}

	async setLnaGain(value) {
		await this.hackrf.setLnaGain(value);
	}

	async setVgaGain(value) {
		await this.hackrf.setVgaGain(value);
	}

	async setFreq(freqHz) {
		await this.hackrf.setFreq(freqHz);
	}

	async setAmpEnable(enable) {
		await this.hackrf.setAmpEnable(enable);
	}

	async setAntennaEnable(enable) {
		await this.hackrf.setAntennaEnable(enable);
	}

	async initSweep(ranges, numBytes, stepWidth, offset, style) {
		await this.hackrf.initSweep(ranges, numBytes, stepWidth, offset, style);
	}

	async startRx(callback) {
		await this.hackrf.startRx(callback);
	}

	async stopRx() {
		await this.hackrf.stopRx();
	}

	async close() {
		await this.hackrf.close();
		await this.hackrf.exit();
	}
}

Comlink.expose(Worker);
