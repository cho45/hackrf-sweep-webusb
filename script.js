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

//const Backend = Comlink.wrap(new Worker("./worker.js", { type: "module" }));
const Backend = Comlink.wrap(new Worker("./worker.js"));

Vue.use(VueMaterial.default);

new Vue({
	el: "#app",
	data: {
		connected: false,
		running: false,
		snackbar: {
			show: false,
			message: ""
		},
		alert: {
			show: false,
			title: "",
			content: ""
		},
		range: {
			start: 2400,
			stop: 2500,
			fftSize: 256
		},
		options: {
			ampEnabled: false,
			antennaEnabled: false,
			lnaGain: 16,
			vgaGain: 16
		},
		info : {
			serialNumber: "",
			boardId: "",
			boardName: "",
			partIdNumber: "",
			firmwareVersion: "",
		},

		currentHover: "",
		showInfo: false
	},

	methods: {
		connect: async function () {
			this.snackbar.show = true;
			this.snackbar.message = "connecting";

			if (!await this.backend.open()) {
				const device = await HackRF.requestDevice();
				if (!device) {
					this.snackbar.message = "device is not found";
					return;
				}
				this.snackbarMessage = "opening device";
				const ok = await this.backend.open({
					vendorId: device.vendorId,
					productId: device.productId,
					serialNumber: device.serialNumber
				});
				if (!ok) {
					this.alert.content = "failed to open device";
					this.alert.show = true;
				}
			}

			this.connected = true;
			const { boardId, versionString, apiVersion, partId, serialNo } = await this.backend.info();

			this.info.serialNumber = serialNo.map( (i) => (i + 0x100000000).toString(16).slice(1) ).join('');
			this.info.boardId = boardId;
			this.info.boardName = HackRF.BOARD_ID_NAME[boardId];
			this.info.firmwareVersion = `${versionString} (API:${apiVersion[0]}.${apiVersion[1]}${apiVersion[2]})`;
			this.info.partIdNumber = partId.map( (i) => (i + 0x100000000).toString(16).slice(1) ).join(' ');
			this.snackbar.message = `connected to ${HackRF.BOARD_ID_NAME[this.info.boardId]}`;
			console.log('apply options', this.options);
			await this.backend.setAmpEnable(this.options.ampEnabled);
			await this.backend.setAntennaEnable(this.options.antennaEnabled);
			await this.backend.setLnaGain(+this.options.lnaGain);
			await this.backend.setVgaGain(+this.options.vgaGain);
		},

		start: async function () {
			if (this.running) return;
			this.running = false;

			const { canvasFft, canvasWf } = this;

			const SAMPLE_RATE = 20e6;

			const lowFreq = +this.range.start;
			const highFreq0 = +this.range.stop;
			const bandwidth0 = highFreq0 - lowFreq;
			const steps = Math.ceil((bandwidth0*1e6) / SAMPLE_RATE);
			const bandwidth = (steps * SAMPLE_RATE) / 1e6;
			const highFreq = lowFreq + bandwidth;
			this.range.stop = highFreq;

			// const FFT_SIZE = +this.range.fftSize;
			// const freqBinCount = (bandwidth*1e6) / SAMPLE_RATE * FFT_SIZE;
			//
			const freqBinCount0 = canvasFft.offsetWidth * window.devicePixelRatio;
			const fftSize0 = Math.pow(2, Math.ceil(Math.log2((freqBinCount0 * SAMPLE_RATE ) / (bandwidth*1e6))));
			const fftSize1 = fftSize0 < +this.range.fftSize ? fftSize0 : +this.range.fftSize;
			const FFT_SIZE = fftSize1 > 8 ? fftSize1 : 8;
			const freqBinCount =  (bandwidth*1e6) / SAMPLE_RATE * FFT_SIZE;
			this.range.fftSize = FFT_SIZE;


			console.log({lowFreq, highFreq, bandwidth, freqBinCount});
			const nx = Math.pow(2, Math.ceil(Math.log2(freqBinCount)));
			const maxTextureSize = 16384;
			const waterfall = (nx <= maxTextureSize) ?
				new WaterfallGL(canvasWf, freqBinCount, 256):
				new Waterfall(canvasWf, freqBinCount, 256);

			canvasFft.height = 200;
			canvasFft.width  = freqBinCount;

			const ctxFft = canvasFft.getContext('2d');

			await this.backend.start({ FFT_SIZE, SAMPLE_RATE, lowFreq, highFreq, bandwidth, freqBinCount }, Comlink.proxy((data) => {
				requestAnimationFrame( () => {
					waterfall.renderLine(data);

					ctxFft.fillStyle = "rgba(0, 0, 0, 0.1)";
					ctxFft.fillRect(0, 0, canvasFft.width, canvasFft.height);
					ctxFft.save();
					ctxFft.beginPath();
					ctxFft.moveTo(0, canvasFft.height);
					for (let i = 0; i < freqBinCount; i++) {
						const n = (data[i] + 45) / 42;
						ctxFft.lineTo(i, canvasFft.height - canvasFft.height * n );
					}
					ctxFft.strokeStyle = "#fff";
					ctxFft.stroke();
					ctxFft.restore();
				});
			}));

			this.running = true;
		},

		stop: async function () {
			this.backend.stopRx();
			this.running = false;
		},

		labelFor: function (n) {
			const lowFreq = +this.range.start;
			const highFreq = +this.range.stop;
			const bandwidth = highFreq - lowFreq;
			const freq = bandwidth * n + lowFreq;
			return (freq).toFixed(1);
		},

		saveSetting: function () {
			const json = JSON.stringify({
				range: this.range,
				options: this.options
			});
			// console.log('saveSetting', json);
			localStorage.setItem('hackrf-sweep-setting', json);
		},

		loadSetting: function () {
			try {
				const json = localStorage.getItem('hackrf-sweep-setting');
				// console.log('loadSetting', json);
				const setting = JSON.parse(json);
				this.range = setting.range;
				this.options = setting.options;
			} catch (e) {
				console.log(e);
			}
		}
	},

	created: async function () {
		this.loadSetting();

		this.backend = await new Backend();
		await this.backend.init();

		this.$watch('options.ampEnabled', async (val) => {
			if (!this.connected) return;
			await this.backend.setAmpEnable(val);
		});

		this.$watch('options.antennaEnabled', async (val) => {
			if (!this.connected) return;
			await this.backend.setAntennaEnable(val);
		});

		this.$watch('options.lnaGain', async (val) => {
			if (!this.connected) return;
			await this.backend.setLnaGain(+val);
		});

		this.$watch('options.vgaGain', async (val) => {
			if (!this.connected) return;
			await this.backend.setVgaGain(+val);
		});

		this.$watch('range', () => {
			this.saveSetting();
		}, { deep: true });

		this.$watch('options', () => {
			this.saveSetting();
		}, { deep: true });

		this.canvasWf = this.$refs.waterfall;
		this.canvasFft = this.$refs.fft;

		const hoverListenr = (e) => {
			const rect = e.currentTarget.getBoundingClientRect();
			const x = e.clientX - rect.x;
			const p = x / rect.width;
			const label = this.labelFor(p);
			this.currentHover = label;
			this.$refs.currentHover.style.left = (p * 100) + "%";
		};

		const leaveListener = (e) => {
			this.$refs.currentHover.style.left = "-100%";
		};

		this.$refs.waterfall.addEventListener('mousemove', hoverListenr);
		this.$refs.waterfall.addEventListener('mouseleave', leaveListener);
		this.$refs.fft.addEventListener('mousemove', hoverListenr);
		this.$refs.fft.addEventListener('mouseleave', leaveListener);

		this.connect();
	},

	mounted: function () {
	}
});

