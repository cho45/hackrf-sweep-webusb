/*
Copyright (c) 2019, cho45 <cho45@lowreal.net>

All rights reserved.

Redistribution and use in source and binary forms, with or without modification, are permitted provided that the following conditions are met:
    Redistributions of source code must retain the above copyright notice, this list of conditions and the following disclaimer.
    Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the following disclaimer in the
    documentation and/or other materials provided with the distribution.
    Neither the name of Great Scott Gadgets nor the names of its contributors may be used to endorse or promote products derived from this software
    without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO,
THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED.
IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
(INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION)
HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
*/

import * as Comlink from "./node_modules/comlink/dist/esm/comlink.mjs";
import { HackRF } from "./hackrf.js";
import init, { FFT } from "./hackrf-web/pkg/hackrf_web.js";

// wasm モジュール（トップレベルでインポート）
console.log('worker: imported');

let wasmInitialized = false;

async function ensureWasmInitialized() {
	if (!wasmInitialized) {
		console.log('worker: loading wasm...');
		await init({ module_or_path: "./hackrf-web/pkg/hackrf_web_bg.wasm" });
		wasmInitialized = true;
		console.log('worker: wasm loaded');
	}
}

class Worker {
	constructor() {
	}

	async init() {
		console.log('init worker');
		await ensureWasmInitialized();
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

		let boardRev = HackRF.BOARD_REV_UNDETECTED;
		try {
			boardRev = await hackrf.boardRevRead();
		} catch (e) {
			console.log(e);
		}

		console.log(`Serial Number: ${serialNo.map( (i) => (i + 0x100000000).toString(16).slice(1) ).join('')}`)
		console.log(`Board ID Number: ${boardId} (${HackRF.BOARD_ID_NAME.get(boardId)})`);
		console.log(`Firmware Version: ${versionString} (API:${apiVersion[0]}.${apiVersion[1]}${apiVersion[2]})`);
		console.log(`Part ID Number: ${partId.map( (i) => (i + 0x100000000).toString(16).slice(1) ).join(' ')}`)
		console.log(`Board Rev: ${HackRF.BOARD_REV_NAME.get(boardRev)} (${boardRev})`)
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

		const BYTES_PER_BLOCK = HackRF.BYTES_PER_BLOCK;

		let startTime = performance.now();
		let prevTime = startTime;
		let readBytes = 0;
		let bytesPerSec = 0;
		let sweepCount = 0;
		let sweepPerSec = 0;

		const fft = new FFT(FFT_SIZE, window);
		fft.set_smoothing_time_constant(0.0);
		const line   = new Float32Array(freqBinCount);
		const output = new Float32Array(FFT_SIZE);
		await hackrf.startRxSweep((data) => {
			readBytes += data.length;
			const now = performance.now();
			const duration = now - prevTime;
			if (duration > 1000) {
				bytesPerSec = readBytes / (duration / 1000);
				prevTime = now;
				readBytes = 0;
			}

			let o = 0;
			for (let n = 0, len = 16; n < len; n++) {
				// console.log(o % HackRF.BYTES_PER_BLOCK, n, data[o+0], data[o+1]);
				if (!(data[o+0] === 0x7F && data[o+1] === 0x7F)) {
					console.log('invalid header', n, data[o+0], data[o+1]);
					o += BYTES_PER_BLOCK;
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
					o += BYTES_PER_BLOCK;
					continue;
				} else
				if (freqM > highFreq) {
					console.log(freqM, 'ignored');
					o += BYTES_PER_BLOCK;
					continue
				} else
				if (freqM === lowFreq) {
					sweepCount++;

					const duration = now - startTime;
					sweepPerSec = sweepCount / (duration / 1000);
					const MAX_FPS = 60;
					if (sweepPerSec < MAX_FPS || sweepCount % Math.round(sweepPerSec / MAX_FPS) === 0) {
						callback(line, { sweepPerSec, bytesPerSec, sweepCount });
					}
					line.fill(0);
				}

				o += BYTES_PER_BLOCK - (FFT_SIZE * 2);
				const target = data.subarray(o, o + FFT_SIZE * 2);
				fft.fft(target, output);
				o += FFT_SIZE * 2;

				//*
				let pos = Math.floor((freqM - lowFreq) / bandwidth * freqBinCount);
				const low = output.subarray(Math.floor(FFT_SIZE/8*1), Math.ceil(FFT_SIZE/8*3) + 1);
				if (pos < line.length) line.set(low.subarray(0, (line.length - pos)), pos);
				const pos2 = pos + FFT_SIZE/2;
				const high = output.subarray(Math.floor(FFT_SIZE/8*5), Math.ceil(FFT_SIZE/8*7) + 1);
				if (pos2 < line.length) line.set(high.subarray(0, (line.length - pos2)), pos2);
				// console.log({freqM, pos, pos2}, output.length, line.length);
				//*/
			}
		});

		console.log('initSweep', [
			[lowFreq, highFreq],
			HackRF.BYTES_PER_BLOCK /* I + Q */,
			SAMPLE_RATE,
			SAMPLE_RATE / 8 * 3,
			HackRF.SWEEP_STYLE_INTERLEAVED
		]);
		await hackrf.initSweep(
			[lowFreq, highFreq],
			HackRF.BYTES_PER_BLOCK /* I + Q */,
			SAMPLE_RATE,
			SAMPLE_RATE / 8 * 3,
			HackRF.SWEEP_STYLE_INTERLEAVED
		);
	}

	async setSampleRateManual(freq, divider) {
		await this.hackrf.setSampleRateManual(freq, divider);
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

	async startRxSweep(callback) {
		await this.hackrf.startRxSweep(callback);
	}

	async stopRx() {
		await this.hackrf.stopRx();
	}

	async close() {
		await this.hackrf.close();
		await this.hackrf.exit();
		await this.hackrf.device.forget();
	}
}

console.log('worker: before Comlink.expose');
Comlink.expose(Worker);
console.log('worker: after Comlink.expose');
