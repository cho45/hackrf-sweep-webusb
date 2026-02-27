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
const FLAVOR = String(process.env.BENCH_FLAVOR ?? "both").toLowerCase();
const CASE_KEYS_RAW = String(process.env.BENCH_CASES ?? "").trim();
const BENCH_VERBOSE_RAW = String(process.env.BENCH_VERBOSE ?? "0").toLowerCase();
const BENCH_VERBOSE =
	BENCH_VERBOSE_RAW === "1" || BENCH_VERBOSE_RAW === "true" || BENCH_VERBOSE_RAW === "on";

const RAW_LOG = console.log.bind(console);
const RUST_LOG_PATTERNS = [/^\[Receiver::new\]/, /^\[CoarseFilter\]/, /^\[DemodFilter\]/, /^\[process#\d+\]/];

const isRustBenchLog = (msg) =>
	typeof msg === "string" && RUST_LOG_PATTERNS.some((pattern) => pattern.test(msg));

function installRustLogFilter() {
	if (BENCH_VERBOSE) {
		return () => {};
	}
	console.log = (...args) => {
		if (args.length > 0 && isRustBenchLog(args[0])) {
			return;
		}
		RAW_LOG(...args);
	};
	return () => {
		console.log = RAW_LOG;
	};
}

const CASES = [
	{ key: "am", label: "AM", demodMode: "AM", ifMinHz: 0, ifMaxHz: 4_500, fmStereo: false, wantFft: true },
	{ key: "am_nofft", label: "AM nofft", demodMode: "AM", ifMinHz: 0, ifMaxHz: 4_500, fmStereo: false, wantFft: false },
	{ key: "fm_mono", label: "FM mono", demodMode: "FM", ifMinHz: 0, ifMaxHz: 98_000, fmStereo: false, wantFft: true },
	{ key: "fm_mono_nofft", label: "FM mono nofft", demodMode: "FM", ifMinHz: 0, ifMaxHz: 98_000, fmStereo: false, wantFft: false },
	{ key: "fm_stereo", label: "FM stereo", demodMode: "FM", ifMinHz: 0, ifMaxHz: 98_000, fmStereo: true, wantFft: true },
	{ key: "fm_stereo_nofft", label: "FM stereo nofft", demodMode: "FM", ifMinHz: 0, ifMaxHz: 98_000, fmStereo: true, wantFft: false },
];
const DEFAULT_CASE_KEYS = new Set(["am", "fm_mono", "fm_stereo"]);

const resolveCases = () => {
	if (!CASE_KEYS_RAW) return CASES.filter((c) => DEFAULT_CASE_KEYS.has(c.key));
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

function printDerived(flavor, metrics) {
	const get = (k) => metrics.get(k)?.msPerBlock;
	const monoNoFft = get("fm_mono_nofft");
	const stereoNoFft = get("fm_stereo_nofft");
	const mono = get("fm_mono");
	const stereo = get("fm_stereo");
	const amNoFft = get("am_nofft");
	const fmt = (v) => (Number.isFinite(v) ? `${v.toFixed(3)} ms` : "n/a");

	if (
		monoNoFft !== undefined ||
		stereoNoFft !== undefined ||
		mono !== undefined ||
		stereo !== undefined ||
		amNoFft !== undefined
	) {
		console.log(`\n[bench:${flavor}:derived]`);
	}
	if (amNoFft !== undefined) {
		console.log(`mix proxy (am_nofft): ${fmt(amNoFft)}`);
	}
	if (monoNoFft !== undefined) {
		console.log(`mix+mono path (fm_mono_nofft): ${fmt(monoNoFft)}`);
	}
	if (monoNoFft !== undefined && stereoNoFft !== undefined) {
		console.log(`stereo extra (nofft): ${fmt(stereoNoFft - monoNoFft)}`);
	}
	if (mono !== undefined && stereo !== undefined) {
		console.log(`stereo extra (with fft): ${fmt(stereo - mono)}`);
	}
	if (mono !== undefined && monoNoFft !== undefined) {
		console.log(`fft extra (fm mono): ${fmt(mono - monoNoFft)}`);
	}
	if (stereo !== undefined && stereoNoFft !== undefined) {
		console.log(`fft extra (fm stereo): ${fmt(stereo - stereoNoFft)}`);
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
			receiver.process_iq_len(IQ_BYTES, c.wantFft);
		});
	}
	await bench.run();
	printBench(flavor, bench);
	const metrics = new Map();
	for (let i = 0; i < bench.tasks.length; i += 1) {
		const task = bench.tasks[i];
		const r = task.result;
		if (!r || !Number.isFinite(r.hz) || r.hz <= 0) continue;
		const key = selectedCases[i]?.key ?? task.name;
		metrics.set(key, {
			hz: r.hz,
			msPerBlock: 1000 / r.hz,
		});
	}
	printDerived(flavor, metrics);
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
	const restoreLog = installRustLogFilter();
	try {
		console.log(`[bench] node=${process.version}`);
		const simdBindings = await canLoadSimd();
		console.log(`[bench] wasm simd support=${simdBindings !== null}`);
		if (BENCH_VERBOSE) {
			console.log("[bench] verbose=true");
		}
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
	} finally {
		restoreLog();
	}
}

main().catch((err) => {
	console.error("[bench] failed:", err);
	process.exitCode = 1;
});
