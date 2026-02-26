import { beforeEach, describe, expect, it, vi } from "vitest";

describe("audio-stream-processor", () => {
  let nowSec = 0;
  let sampleRateHz = 1_000;
  let registerProcessorMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    vi.resetModules();
    nowSec = 0;
    sampleRateHz = 1_000;
    registerProcessorMock = vi.fn();

    class AudioWorkletProcessorMock {
      port = {
        onmessage: null as ((event: MessageEvent) => void) | null,
        postMessage: vi.fn(),
      };
    }

    Object.defineProperty(globalThis, "currentTime", {
      configurable: true,
      get: () => nowSec,
    });
    Object.defineProperty(globalThis, "sampleRate", {
      configurable: true,
      get: () => sampleRateHz,
    });
    Object.defineProperty(globalThis, "AudioWorkletProcessor", {
      configurable: true,
      value: AudioWorkletProcessorMock,
    });
    Object.defineProperty(globalThis, "registerProcessor", {
      configurable: true,
      value: registerProcessorMock,
    });
  });

  it("registers processor and reports underrun + gap peak stats", async () => {
    const mod = await import("./audio-stream-processor");
    expect(registerProcessorMock).toHaveBeenCalledWith(
      "audio-stream-processor",
      mod.AudioStreamProcessor,
    );

    const processor = new mod.AudioStreamProcessor() as unknown as {
      port: {
        onmessage: ((event: MessageEvent) => void) | null;
        postMessage: ReturnType<typeof vi.fn>;
      };
      process: (inputs: Float32Array[][], outputs: Float32Array[][]) => boolean;
    };

    const inputPort = {
      onmessage: null as ((event: MessageEvent) => void) | null,
      start: vi.fn(),
      close: vi.fn(),
      postMessage: vi.fn(),
    } as unknown as MessagePort;
    processor.port.onmessage?.(
      new MessageEvent("message", {
        data: { type: "attach-input-port", port: inputPort },
      }),
    );

    const push = (data: Float32Array) => {
      (inputPort.onmessage as ((event: MessageEvent) => void) | null)?.(
        new MessageEvent("message", { data: { type: "push", data } }),
      );
    };

    const processOnce = () => {
      const out = new Float32Array(128);
      processor.process([], [[out]]);
    };

    push(new Float32Array(256).fill(0.25));
    nowSec = 0.01;
    processOnce();
    nowSec = 0.02;
    processOnce();
    nowSec = 0.03;
    processOnce(); // queue empty -> underrun

    nowSec = 0.11;
    push(new Float32Array(128).fill(0.5)); // gap peak: about 110ms
    nowSec = 0.12;
    processOnce();

    nowSec = 0.7; // stats window >= 0.5s
    processOnce();

    const messages = processor.port.postMessage.mock.calls.map(
      (args: unknown[]) => args[0] as Record<string, unknown>,
    );
    const stats = messages.find((m) => m.type === "stats");
    expect(stats).toBeDefined();
    expect((stats?.underrunCount as number) >= 1).toBe(true);
    expect((stats?.inputGapMsPeak as number) >= 100).toBe(true);
    expect((stats?.bufferedMs as number) >= 0).toBe(true);
  });

  it("counts dropped samples when input burst exceeds max buffer", async () => {
    const mod = await import("./audio-stream-processor");
    const processor = new mod.AudioStreamProcessor() as unknown as {
      port: {
        onmessage: ((event: MessageEvent) => void) | null;
        postMessage: ReturnType<typeof vi.fn>;
      };
      process: (inputs: Float32Array[][], outputs: Float32Array[][]) => boolean;
    };

    const inputPort = {
      onmessage: null as ((event: MessageEvent) => void) | null,
      start: vi.fn(),
      close: vi.fn(),
      postMessage: vi.fn(),
    } as unknown as MessagePort;
    processor.port.onmessage?.(
      new MessageEvent("message", {
        data: { type: "attach-input-port", port: inputPort },
      }),
    );

    (inputPort.onmessage as ((event: MessageEvent) => void) | null)?.(
      new MessageEvent("message", {
        data: { type: "push", data: new Float32Array(5_000).fill(1.0) },
      }),
    );

    nowSec = 0.6;
    processor.process([], [[new Float32Array(128)]]);

    const messages = processor.port.postMessage.mock.calls.map(
      (args: unknown[]) => args[0] as Record<string, unknown>,
    );
    const stats = messages.find((m) => m.type === "stats");
    expect(stats).toBeDefined();
    expect((stats?.droppedSamplesCount as number) > 0).toBe(true);
  });
});
