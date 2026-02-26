import { describe, it, expect, vi } from 'vitest';
import { HackRF } from './hackrf';

type TransferResult = {
	status: USBTransferStatus;
	data: DataView | null;
};

type TransferResolver = (result: TransferResult) => void;

const createMockDevice = () => {
	const pending: TransferResolver[] = [];
	const controlTransferOut = vi.fn(async () => ({ status: 'ok' as USBTransferStatus }));
	const transferIn = vi.fn(
		() =>
			new Promise<TransferResult>((resolve) => {
				pending.push(resolve);
			})
	);

	const device = {
		controlTransferOut,
		transferIn,
	} as unknown as USBDevice;

	return { device, pending, controlTransferOut, transferIn };
};

const makeTransfer = (bytes: number[], byteOffset = 0, byteLength = bytes.length): TransferResult => {
	const buf = new Uint8Array(bytes).buffer;
	return {
		status: 'ok',
		data: new DataView(buf, byteOffset, byteLength),
	};
};

describe('HackRF Rx lifecycle', () => {
	it('stopRx keeps recovery path even if transfer promises reject', async () => {
		const { device, controlTransferOut } = createMockDevice();
		const hackrf = new HackRF();
		(hackrf as any).device = device;
		(hackrf as any).rxRunning = [Promise.reject(new Error('rx failed')), Promise.resolve()];

		await expect(hackrf.stopRx()).resolves.toBeUndefined();
		expect(controlTransferOut).toHaveBeenCalledWith(
			expect.objectContaining({
				request: HackRF.HACKRF_VENDOR_REQUEST_SET_TRANSCEIVER_MODE,
				value: HackRF.HACKRF_TRANSCEIVER_MODE_OFF,
			})
		);
	});

	it('startRx passes transfer bytes using DataView byteOffset', async () => {
		const { device, pending } = createMockDevice();
		const hackrf = new HackRF();
		(hackrf as any).device = device;

		let received: Uint8Array | null = null;
		let stopPromise: Promise<void> | null = null;

		await hackrf.startRx((data) => {
			if (!received) {
				received = new Uint8Array(data);
				stopPromise = hackrf.stopRx();
			}
		});

		await Promise.resolve();
		expect(pending.length).toBe(HackRF.RX_TRANSFER_IN_FLIGHT);

		// DataView has non-zero offset: expected payload is [1, 2, 3, 4]
		const packet = makeTransfer([9, 8, 1, 2, 3, 4], 2, 4);
		while (pending.length > 0) {
			pending.shift()!(packet);
		}

		await stopPromise;
		expect(received).not.toBeNull();
		expect(Array.from(received!)).toEqual([1, 2, 3, 4]);
	});

	it('startRx keeps stop/restart path alive when callback throws', async () => {
		const { device, pending } = createMockDevice();
		const hackrf = new HackRF();
		(hackrf as any).device = device;

		await hackrf.startRx(() => {
			throw new Error('callback failed');
		});

		await Promise.resolve();
		expect(pending.length).toBe(HackRF.RX_TRANSFER_IN_FLIGHT);

		const packet = makeTransfer([1, 2, 3, 4]);
		while (pending.length > 0) {
			pending.shift()!(packet);
		}

		await expect(hackrf.stopRx()).resolves.toBeUndefined();
	});
});
