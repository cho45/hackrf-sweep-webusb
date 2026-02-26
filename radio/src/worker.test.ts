import { describe, it, expect, vi, beforeEach } from 'vitest';

const hoisted = vi.hoisted(() => ({
	expose: vi.fn(),
	init: vi.fn(async () => ({ memory: { buffer: new ArrayBuffer(1024 * 1024) } })),
	receiverCtor: vi.fn(),
	mockHackrfInstances: [] as any[],
}));

vi.mock('comlink', () => ({
	expose: hoisted.expose,
}));

const createWasmMockModule = () => {
	class Receiver {
		constructor(...args: any[]) {
			hoisted.receiverCtor(...args);
		}
		alloc_io_buffers() {}
		free_io_buffers() {}
		iq_input_ptr() { return 0; }
		audio_output_ptr() { return 1024; }
		fft_output_ptr() { return 2048; }
		iq_input_capacity() { return 262144; }
		audio_output_capacity() { return 4096; }
		fft_output_capacity() { return 1024; }
		audio_output_channels() { return 1; }
			process_iq_len(_iqLen: number) {
				return 0;
			}
		get_stats() {
			return {
				pilot_level: 0,
				stereo_blend: 0,
				stereo_locked: false,
				mono_fallback_count: 0,
			};
		}
		set_fm_stereo_enabled() {}
		free() {}
	}

	return {
		default: hoisted.init,
		Receiver,
	};
};

vi.mock('../hackrf-dsp/pkg/hackrf_dsp', () => {
	return createWasmMockModule();
});

vi.mock('../hackrf-dsp/pkg-simd/hackrf_dsp', () => {
	return createWasmMockModule();
});

vi.mock('./hackrf', () => {
	class HackRF {
		open = vi.fn(async () => {});
		close = vi.fn(async () => {});
		exit = vi.fn(async () => {});
		stopRx = vi.fn(async () => {});
		setAmpEnable = vi.fn(async () => {});
		setAntennaEnable = vi.fn(async () => {});
		setLnaGain = vi.fn(async () => {});
		setVgaGain = vi.fn(async () => {});
		setSampleRateManual = vi.fn(async () => {});
		setFreq = vi.fn(async () => {});
		startRx = vi.fn(async () => {});

		constructor() {
			hoisted.mockHackrfInstances.push(this);
		}
	}

	return { HackRF };
});

const createMockDevice = () => ({
	open: vi.fn(async () => {}),
	close: vi.fn(async () => {}),
	exit: vi.fn(async () => {}),
	stopRx: vi.fn(async () => {}),
	setAmpEnable: vi.fn(async () => {}),
	setAntennaEnable: vi.fn(async () => {}),
	setLnaGain: vi.fn(async () => {}),
	setVgaGain: vi.fn(async () => {}),
	setSampleRateManual: vi.fn(async () => {}),
	setFreq: vi.fn(async () => {}),
	startRx: vi.fn(async () => {}),
});

describe('RadioBackend', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		hoisted.mockHackrfInstances.length = 0;
	});

	it('re-applies RF front-end settings on every startRx', async () => {
		const { RadioBackend } = await import('./worker');
		const backend = new RadioBackend();
		(backend as any).wasmModule = { memory: { buffer: new ArrayBuffer(1024 * 1024) } };
		const device = createMockDevice();
		(backend as any).device = device;

		await backend.startRx(
			{
				sampleRate: 2_000_000,
				centerFreq: 1_250_000,
				targetFreq: 1_000_000,
				demodMode: 'AM',
				outputSampleRate: 48_000,
					fftSize: 1024,
					fftVisibleStartBin: 0,
					fftVisibleBins: 512,
					ifMinHz: 0,
					ifMaxHz: 4_500,
					dcCancelEnabled: true,
					fmStereoEnabled: true,
					ampEnabled: true,
					antennaEnabled: true,
					lnaGain: 24,
					vgaGain: 32,
				},
			() => {}
		);

		expect(device.stopRx).toHaveBeenCalled();
		expect(device.setAmpEnable).toHaveBeenCalledWith(true);
		expect(device.setAntennaEnable).toHaveBeenCalledWith(true);
		expect(device.setLnaGain).toHaveBeenCalledWith(24);
		expect(device.setVgaGain).toHaveBeenCalledWith(32);
		expect(device.setSampleRateManual).toHaveBeenCalledWith(2_000_000, 1);
		expect(device.setFreq).toHaveBeenCalledWith(1_250_000);
		expect(device.startRx).toHaveBeenCalled();
	});

	it('forces RX OFF right after open', async () => {
		const { RadioBackend } = await import('./worker');
		const backend = new RadioBackend();

		const usbDevice = { vendorId: 0x1d50, productId: 0x6089 } as USBDevice;
		Object.defineProperty(navigator, 'usb', {
			value: {
				getDevices: vi.fn(async () => [usbDevice]),
			},
			configurable: true,
		});

		const ok = await backend.open();
		expect(ok).toBe(true);
		expect(hoisted.mockHackrfInstances).toHaveLength(1);
		const instance = hoisted.mockHackrfInstances[0];
		expect(instance.open).toHaveBeenCalledWith(usbDevice);
		expect(instance.stopRx).toHaveBeenCalled();
	});
});
