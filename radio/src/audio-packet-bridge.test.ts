import { describe, expect, it } from "vitest";
import {
	AudioPacketReceiver,
	AudioPacketSender,
	type AudioPushMessage,
} from "./audio-packet-bridge";

describe("audio-packet-bridge", () => {
	it("packs variable-length audio into fixed-size packets", () => {
		const sender = new AudioPacketSender(8, 4);
		const sent: AudioPushMessage[] = [];
		const port = {
			postMessage: (msg: AudioPushMessage) => {
				sent.push(msg);
			},
		};

		sender.appendFrom(new Float32Array([1, 2, 3, 4, 5, 6]), 6, 2, port);
		expect(sent).toHaveLength(0);
		sender.appendFrom(new Float32Array([7, 8]), 2, 2, port);

		expect(sent).toHaveLength(1);
		expect(Array.from(sent[0]!.data)).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);
	});

	it("tracks dropped samples when sender pool is exhausted", () => {
		const sender = new AudioPacketSender(4, 1);
		const sent: AudioPushMessage[] = [];
		const port = {
			postMessage: (msg: AudioPushMessage) => {
				sent.push(msg);
			},
		};

		sender.appendFrom(new Float32Array([1, 2, 3, 4, 5, 6, 7, 8]), 8, 1, port);
		expect(sent).toHaveLength(1);
		expect(sender.getDroppedSamplesCount()).toBe(4);
	});

	it("drops oldest receiver frames and reports dropped samples", () => {
		const receiver = new AudioPacketReceiver();
		const recycled: number[] = [];

		expect(
			receiver.pushFromMessage({
				type: "push",
				channels: 2,
				data: new Float32Array([1, 10, 2, 20, 3, 30, 4, 40]),
			}),
		).toBe(true);
		expect(receiver.getBufferedFrames()).toBe(4);

		const droppedSamples = receiver.dropOldFrames(2, (data) => recycled.push(data.length));
		expect(droppedSamples).toBe(4);
		expect(recycled).toEqual([]);
		expect(receiver.getBufferedFrames()).toBe(2);

		const outL = new Float32Array(2);
		const outR = new Float32Array(2);
		const drained = receiver.drainInto(outL, outR, (data) => recycled.push(data.length));
		expect(drained.writtenFrames).toBe(2);
		expect(Array.from(outL)).toEqual([3, 4]);
		expect(Array.from(outR)).toEqual([30, 40]);
		expect(recycled).toEqual([8]);
	});

	it("recycles transferred buffers between sender and receiver over MessagePort", async () => {
		const sender = new AudioPacketSender(4, 1);
		const receiver = new AudioPacketReceiver();
		const ch = new MessageChannel();
		if (typeof ch.port1.start === "function") ch.port1.start();
		if (typeof ch.port2.start === "function") ch.port2.start();

		const played: number[][] = [];
		let sentSecond = false;
		const normalizeFloat32 = (value: unknown): Float32Array | null => {
			if (value instanceof Float32Array) return value;
			if (value instanceof ArrayBuffer) return new Float32Array(value);
			if (ArrayBuffer.isView(value)) {
				const view = value as ArrayBufferView;
				return new Float32Array(view.buffer, view.byteOffset, Math.floor(view.byteLength / 4));
			}
			return null;
		};

		const done = new Promise<void>((resolve, reject) => {
			ch.port1.onmessage = (ev: MessageEvent) => {
				try {
					const msg = ev.data as { type?: string; data?: unknown };
					expect(msg.type).toBe("recycle");
					const recycled = normalizeFloat32(msg.data);
					expect(recycled).not.toBeNull();
					expect(sender.recycle(recycled!)).toBe(true);
					if (!sentSecond) {
						sender.appendFrom(new Float32Array([5, 6, 7, 8]), 4, 1, ch.port1);
						sentSecond = true;
					}
				} catch (e) {
					reject(e);
				}
			};

			ch.port2.onmessage = (ev: MessageEvent) => {
				try {
					const msg = ev.data as AudioPushMessage;
					expect(msg.type).toBe("push");
					expect(receiver.pushFromMessage(msg)).toBe(true);
					const out = new Float32Array(4);
					const drained = receiver.drainInto(out, out, (data) => {
						ch.port2.postMessage({ type: "recycle", data }, [data.buffer]);
					});
					expect(drained.writtenFrames).toBe(4);
					played.push(Array.from(out));
					if (played.length >= 2) {
						resolve();
					}
				} catch (e) {
					reject(e);
				}
			};
		});

		sender.appendFrom(new Float32Array([1, 2, 3, 4]), 4, 1, ch.port1);
		await done;

		expect(played).toEqual([
			[1, 2, 3, 4],
			[5, 6, 7, 8],
		]);
		expect(sender.getDroppedSamplesCount()).toBe(0);

		ch.port1.close();
		ch.port2.close();
	});
});

