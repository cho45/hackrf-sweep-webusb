#!/usr/bin/env node
import { readFile } from "node:fs/promises";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath, pathToFileURL } from "node:url";

const SIMD_PROBE_WASM = new Uint8Array([
	0x00, 0x61, 0x73, 0x6d,
	0x01, 0x00, 0x00, 0x00,
	0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f,
	0x03, 0x02, 0x01, 0x00,
	0x07, 0x05, 0x01, 0x01, 0x66, 0x00, 0x00,
	0x0a, 0x19, 0x01, 0x17, 0x00,
	0xfd, 0x0c,
	0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
	0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
	0xfd, 0x15, 0x00,
	0x0b,
]);

const HERE = path.dirname(fileURLToPath(import.meta.url));
const RADIO_ROOT = path.resolve(HERE, "..");
const DSP_ROOT = path.join(RADIO_ROOT, "hackrf-dsp");

const LOOPS = Number(process.env.BENCH_LOOPS ?? 3000);
const WARMUP = Number(process.env.BENCH_WARMUP ?? 300);
const IQ_BYTES = Number(process.env.BENCH_IQ_BYTES ?? 262_144);
const SAMPLE_RATE = Number(process.env.BENCH_SR ?? 20_000_000);
const AUDIO_SR = Number(process.env.BENCH_AUDIO_SR ?? 48_000);
const DEMOD_MODE = String(process.env.BENCH_MODE ?? "FM");
const FFT_SIZE = Number(process.env.BENCH_FFT_SIZE ?? 1024);
const CENTER_FREQ = Number(process.env.BENCH_CENTER ?? 100_000_000);
const TARGET_FREQ = Number(process.env.BENCH_TARGET ?? CENTER_FREQ);
const IF_MIN_HZ = Number(process.env.BENCH_IF_MIN ?? (DEMOD_MODE === "AM" ? -25_000 : -100_000));
const IF_MAX_HZ = Number(process.env.BENCH_IF_MAX ?? (DEMOD_MODE === "AM" ? 25_000 : 100_000));
const DC_CANCEL = String(process.env.BENCH_DC ?? "off").toLowerCase() === "on";
const FFT_USE_PROCESSED = String(process.env.BENCH_FFT_PROCESSED ?? "off").toLowerCase() === "on";

function supportsWasmSimd() {
	if (typeof WebAssembly === "undefined" || typeof WebAssembly.validate !== "function") {
		return false;
	}
	try {
		return WebAssembly.validate(SIMD_PROBE_WASM);
	} catch {
		return false;
	}
}

function fillIq(buf) {
	let phase = 0;
	const dphase = 0.0314159265;
	for (let i = 0; i < buf.length; i += 2) {
		buf[i] = (Math.sin(phase) * 96) | 0;
		buf[i + 1] = (Math.cos(phase) * 96) | 0;
		phase += dphase;
	}
}

async function loadBindings(flavor) {
	const pkgDir = flavor === "simd" ? path.join(DSP_ROOT, "pkg-simd") : path.join(DSP_ROOT, "pkg");
	const jsPath = path.join(pkgDir, "hackrf_dsp.js");
	const wasmPath = path.join(pkgDir, "hackrf_dsp_bg.wasm");
	const mod = await import(pathToFileURL(jsPath).href);
	const wasmBytes = await readFile(wasmPath);
	const wasm = await mod.default({ module_or_path: wasmBytes });
	return { Receiver: mod.Receiver, wasm };
}

function benchOnce(flavor, bindings) {
	const receiver = new bindings.Receiver(
		SAMPLE_RATE,
		CENTER_FREQ,
		TARGET_FREQ,
		DEMOD_MODE,
		AUDIO_SR,
		FFT_SIZE,
		0,
		FFT_SIZE,
		IF_MIN_HZ,
		IF_MAX_HZ,
		DC_CANCEL,
		FFT_USE_PROCESSED,
	);

	receiver.alloc_io_buffers(IQ_BYTES, 65_536, FFT_SIZE);
	const iq = new Int8Array(bindings.wasm.memory.buffer, receiver.iq_input_ptr(), IQ_BYTES);
	fillIq(iq);

	let audioSamples = 0;
	for (let i = 0; i < WARMUP; i += 1) {
		audioSamples += receiver.process_iq_len(IQ_BYTES);
	}

	const t0 = performance.now();
	for (let i = 0; i < LOOPS; i += 1) {
		audioSamples += receiver.process_iq_len(IQ_BYTES);
	}
	const elapsedMs = performance.now() - t0;

	receiver.free_io_buffers();
	receiver.free();

	const msPerBlock = elapsedMs / LOOPS;
	const blocksPerSec = 1000.0 / msPerBlock;
	const iqMBps = (IQ_BYTES * blocksPerSec) / (1024 * 1024);

	return {
		flavor,
		msPerBlock,
		blocksPerSec,
		iqMBps,
		audioSamples,
	};
}

function printResult(result) {
	console.log(
		`${result.flavor}: ${result.msPerBlock.toFixed(3)} ms/block, ${result.blocksPerSec.toFixed(1)} blocks/s, ${result.iqMBps.toFixed(2)} MB/s`,
	);
}

async function main() {
	console.log(`[bench] node=${process.version}`);
	console.log(`[bench] loops=${LOOPS} warmup=${WARMUP} iq_bytes=${IQ_BYTES}`);

	const simdSupported = supportsWasmSimd();
	console.log(`[bench] wasm simd support=${simdSupported}`);
	if (!simdSupported) {
		console.log("[bench] skip: Node runtime does not support wasm simd128");
		return;
	}

	const base = benchOnce("base", await loadBindings("base"));
	const simd = benchOnce("simd", await loadBindings("simd"));
	printResult(base);
	printResult(simd);
	console.log(`[bench] speedup(simd/base): ${(base.msPerBlock / simd.msPerBlock).toFixed(3)}x`);
}

main().catch((err) => {
	console.error("[bench] failed:", err);
	process.exitCode = 1;
});
