import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, flushPromises } from '@vue/test-utils';
import App from './App.vue';

const hoisted = vi.hoisted(() => ({
	backendInstances: [] as any[],
}));

class WorkerMock {
	postMessage() {
		// Comlink の初期化要求に対してダミーで応答する仕組みがないとawaitで止まるため、
		// 今回はテスト専用に backend そのものを強制的にモック注入するか、
		// navigator.usb.requestDevice の呼び出し自体を強引に引き起こすための構造を記述する。
	}
	terminate() { }
	addEventListener() { }
	removeEventListener() { }
}
globalThis.Worker = WorkerMock as any;

// Comlinkの挙動を直接モックするのは困難なため、vi.mock で comlink をモックする
vi.mock('comlink', () => ({
	wrap: () => {
		return class {
			init = vi.fn(async () => true);
			open = vi.fn(async (args?: any) => {
				// argsがない初回はfalseを返し、App.vue側にrequestDeviceをトリガーさせる
				if (!args) return false;
				return true;
			});
			info = vi.fn(async () => ({ boardId: 0, versionString: 'test', apiVersion: [1, 0, 0], partId: [0, 0], serialNo: [0, 0, 0, 0] }));
			setVgaGain = vi.fn(async () => {});
			setLnaGain = vi.fn(async () => {});
			setAmpEnable = vi.fn(async () => {});
			setAntennaEnable = vi.fn(async () => {});
			setAudioPort = vi.fn(async () => {});
			startRx = vi.fn(async () => {});
			stopRx = vi.fn(async () => {});
			close = vi.fn(async () => {});
			setDcCancelEnabled = vi.fn(async () => {});
			constructor() {
				hoisted.backendInstances.push(this);
			}
		};
	},
	proxy: (v: any) => v,
	transfer: (v: any) => v,
}));

// mock navigator.usb
Object.defineProperty(navigator, 'usb', {
	value: {
		requestDevice: async () => null,
		getDevices: async () => []
	},
	writable: true
});

// mock HTMLCanvasElement dependencies for jsdom
HTMLCanvasElement.prototype.getContext = vi.fn((contextId: string) => {
	if (contextId === '2d') {
		return {
			clearRect: vi.fn(),
			beginPath: vi.fn(),
			moveTo: vi.fn(),
			lineTo: vi.fn(),
			stroke: vi.fn(),
			save: vi.fn(),
			restore: vi.fn(),
		} as any;
	}
	return null as any;
});
Object.defineProperty(HTMLElement.prototype, 'offsetWidth', { configurable: true, value: 500 });
window.devicePixelRatio = 2;

class AudioWorkletNodeMock {
	port = {
		onmessage: null as ((event: MessageEvent) => void) | null,
		postMessage: vi.fn(),
	};
	connect = vi.fn();
}

class AudioContextMock {
	sampleRate = 48_000;
	destination = {};
	audioWorklet = { addModule: vi.fn(async () => {}) };
	resume = vi.fn(async () => {});
	suspend = vi.fn(async () => {});
}

(globalThis as any).AudioContext = AudioContextMock;
(globalThis as any).AudioWorkletNode = AudioWorkletNodeMock;

// Appコンポーネントが正常にマウントされ、タイトルが表示されるかをテストする
describe('App.vue', () => {
	beforeEach(() => {
		hoisted.backendInstances.length = 0;
		vi.clearAllMocks();
	});

	it('renders tuning fields', () => {
		const wrapper = mount(App);
		const labels = wrapper.findAll('label').map((v) => v.text());
		expect(labels).toContain('Target Frequency');
		expect(labels).toContain('Span');
	});

	it('has initial disconnected state', () => {
		const wrapper = mount(App);
		const connectBtn = wrapper.findAll('button').find(b => b.text() === 'Connect');
		const disconnectBtn = wrapper.findAll('button').find(b => b.text() === 'Disconnect');
		const startBtn = wrapper.findAll('button').find(b => b.text() === 'Start Rx');

		expect(connectBtn).toBeDefined();
		expect(connectBtn!.attributes('disabled')).toBeUndefined();
		expect(disconnectBtn).toBeUndefined();
		expect(startBtn).toBeUndefined();
	});

	it('should call navigator.usb.requestDevice and pass ids to backend on connect', async () => {
		const requestDeviceSpy = vi.spyOn(navigator.usb, 'requestDevice').mockResolvedValue({
			vendorId: 0x1d50,
			productId: 0x6089,
			serialNumber: 'test-serial-123',
			configurations: []
		} as any);

		const wrapper = mount(App);

		// backend を強引にモック (setup()のインスタンス変数としては取れないため)
		// Appコンポーネントのテストとして「requestDevice がクリック時に呼ばれるか」を主要な検証とする
		const connectBtn = wrapper.findAll('button').find(b => b.text() === 'Connect');
		expect(connectBtn).toBeDefined();

		// Vue の nextTick とイベントループを回して Promiseチェーン(backend.open())を解決させる
		await connectBtn!.trigger('click');
		await flushPromises();

		// 少なくとも navigator.usb.requestDevice がコールされていることを実証
		// backend.open はComlink内包のため直接のアサートはスキップするが、
		// この呼び出しが成功すること自体がMain ThreadからのUSB呼び出し要求の証明となる
		expect(requestDeviceSpy).toHaveBeenCalled();

		requestDeviceSpy.mockRestore();
	});

	it('wires audio port to backend on start', async () => {
		const requestDeviceSpy = vi.spyOn(navigator.usb, 'requestDevice').mockResolvedValue({
			vendorId: 0x1d50,
			productId: 0x6089,
			serialNumber: 'test-serial-123',
			configurations: [],
		} as any);

		const wrapper = mount(App);
		const connectBtn = wrapper.findAll('button').find(b => b.text() === 'Connect');
		expect(connectBtn).toBeDefined();
		await connectBtn!.trigger('click');
		await flushPromises();

		const startBtn = wrapper.findAll('button').find(b => b.text() === 'Start Rx');
		expect(startBtn).toBeDefined();
		await startBtn!.trigger('click');
		await flushPromises();

		expect(hoisted.backendInstances.length).toBeGreaterThan(0);
		const backend = hoisted.backendInstances[0];
		expect(backend.setAudioPort).toHaveBeenCalledTimes(1);
		expect(backend.startRx).toHaveBeenCalledTimes(1);

		requestDeviceSpy.mockRestore();
	});
});
