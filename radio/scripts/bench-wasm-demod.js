#!/usr/bin/env node
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { Bench } from "tinybench";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const RADIO_ROOT = path.resolve(HERE, "..");
const DSP_ROOT = path.join(RADIO_ROOT, "hackrf-dsp");

const SAMPLE_RATE = Number(process.env.BENCH_SR ?? 20_000_000);
const AUDIO_SR = Number(process.env.BENCH_AUDIO_SR ?? 48_000);
const IQ_BYTES = Number(process.env.BENCH_IQ_BYTES ?? 262_144);
const FFT_SIZE = Number(process.env.BENCH_FFT_SIZE ?? 1024);
const BENCH_TIME_MS = Number(process.env.BENCH_TIME_MS ?? 1200);
const BENCH_WARMUP_MS = Number(process.env.BENCH_WARMUP_MS ?? 300);
const FLAVOR = String(process.env.BENCH_FLAVOR ?? "simd").toLowerCase();
const CASE_KEYS_RAW = String(process.env.BENCH_CASES ?? "").trim();

const CASES = [
	{ key: "am", label: "AM", demodMode: "AM", ifMinHz: 0, ifMaxHz: 4_500, fmStereo: false },
	{ key: "fm_mono", label: "FM mono", demodMode: "FM", ifMinHz: 0, ifMaxHz: 98_000, fmStereo: false },
	{ key: "fm_stereo", label: "FM stereo", demodMode: "FM", ifMinHz: 0, ifMaxHz: 98_000, fmStereo: true },
];

const resolveCases = () => {
	if (!CASE_KEYS_RAW) return CASES;
	const requested = new Set(
		CASE_KEYS_RAW
			.split(",")
			.map((s) => s.trim().toLowerCase())
			.filter(Boolean),
	);
	const selected = CASES.filter((c) => requested.has(c.key));
	if (selected.length === 0) {
		throw new Error(
			`invalid BENCH_CASES=${CASE_KEYS_RAW} (allowed: ${CASES.map((c) => c.key).join(",")})`,
		);
	}
	return selected;
};

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

function setupReceiver(bindings, c) {
	const receiver = new bindings.Receiver(
		SAMPLE_RATE,
		100_000_000,
		100_000_000,
		c.demodMode,
		AUDIO_SR,
		FFT_SIZE,
		0,
		FFT_SIZE,
		c.ifMinHz,
		c.ifMaxHz,
		true,
	);
	receiver.set_fm_stereo_enabled(c.fmStereo);
	receiver.alloc_io_buffers(IQ_BYTES, 65_536, FFT_SIZE);
	const iq = new Int8Array(bindings.wasm.memory.buffer, receiver.iq_input_ptr(), IQ_BYTES);
	fillIq(iq);
	return receiver;
}

function freeReceiver(receiver) {
	receiver.free_io_buffers();
	receiver.free();
}

function computeRealtimeTarget() {
	const iqSamplesPerBlock = IQ_BYTES / 2;
	const requiredBlocksPerSec = SAMPLE_RATE / iqSamplesPerBlock;
	return {
		requiredBlocksPerSec,
		blockBudgetMs: 1000 / requiredBlocksPerSec,
	};
}

function printBench(flavor, bench) {
	const rt = computeRealtimeTarget();
	console.log(`\n[bench:${flavor}] sampleRate=${SAMPLE_RATE} audioRate=${AUDIO_SR} iqBytes=${IQ_BYTES}`);
	console.log(
		`[bench:${flavor}] realtime target=${rt.requiredBlocksPerSec.toFixed(1)} blocks/s  budget=${rt.blockBudgetMs.toFixed(3)} ms/block`,
	);
	for (const task of bench.tasks) {
		const r = task.result;
		if (!r) continue;
		const hz = r.hz;
		const ms = 1000 / hz;
		const rme = Number.isFinite(r.rme) ? r.rme : 0;
		const rtMargin = hz / rt.requiredBlocksPerSec;
		const headroomPct = (rtMargin - 1) * 100;
		console.log(
			`${task.name.padEnd(10)}  ${ms.toFixed(3)} ms/block  ${hz.toFixed(1)} blocks/s  rt=${rtMargin.toFixed(2)}x (${headroomPct.toFixed(0)}%)  ±${rme.toFixed(2)}%`,
		);
	}
}

async function runFlavor(flavor) {
	const bindings = await loadBindings(flavor);
	const bench = new Bench({
		time: BENCH_TIME_MS,
		warmupTime: BENCH_WARMUP_MS,
	});
	const selectedCases = resolveCases();
	const receivers = [];
	for (const c of selectedCases) {
		const receiver = setupReceiver(bindings, c);
		receivers.push(receiver);
		bench.add(c.label, () => {
			receiver.process_iq_len(IQ_BYTES);
		});
	}
	await bench.run();
	printBench(flavor, bench);
	const metrics = new Map();
	for (const task of bench.tasks) {
		const r = task.result;
		if (!r || !Number.isFinite(r.hz) || r.hz <= 0) continue;
		metrics.set(task.name, {
			hz: r.hz,
			msPerBlock: 1000 / r.hz,
		});
	}
	for (const receiver of receivers) {
		freeReceiver(receiver);
	}
	return metrics;
}

async function canLoadSimd() {
	try {
		const bindings = await loadBindings("simd");
		return bindings;
	} catch {
		return null;
	}
}

async function main() {
	console.log(`[bench] node=${process.version}`);
	const simdBindings = await canLoadSimd();
	console.log(`[bench] wasm simd support=${simdBindings !== null}`);
	if (CASE_KEYS_RAW) {
		console.log(`[bench] cases=${CASE_KEYS_RAW}`);
	}
	if (FLAVOR === "both") {
		const baseMetrics = await runFlavor("base");
		if (simdBindings) {
			const simdMetrics = await runFlavor("simd");
			console.log("\n[speedup simd/base]");
			for (const [name, base] of baseMetrics.entries()) {
				const simd = simdMetrics.get(name);
				if (!simd || simd.msPerBlock <= 0) continue;
				const speedup = base.msPerBlock / simd.msPerBlock;
				console.log(`${name.padEnd(10)}  ${speedup.toFixed(3)}x`);
			}
		}
		return;
	}
	if (FLAVOR === "simd") {
		if (!simdBindings) {
			console.log("[bench] skip simd: runtime does not support wasm simd128");
			return;
		}
		await runFlavor("simd");
		return;
	}
	await runFlavor("base");
}

main().catch((err) => {
	console.error("[bench] failed:", err);
	process.exitCode = 1;
});
