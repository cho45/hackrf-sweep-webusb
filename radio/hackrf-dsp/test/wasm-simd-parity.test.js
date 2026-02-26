// @vitest-environment node
import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { describe, it, expect } from "vitest";

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
const DSP_ROOT = path.resolve(HERE, "..");

const BLOCKS = Number(process.env.PARITY_BLOCKS ?? 80);
const IQ_CAP = Number(process.env.PARITY_IQ_CAP ?? 262_144);
const AUDIO_CAP = Number(process.env.PARITY_AUDIO_CAP ?? 65_536);
const EPS_FFT = Number(process.env.PARITY_EPS_FFT ?? 1e-3);
const AUDIO_MAX_LAG = Number(process.env.PARITY_AUDIO_MAX_LAG ?? 2);
const AUDIO_RMSE_RATIO_MAX = Number(process.env.PARITY_AUDIO_RMSE_RATIO_MAX ?? 0.12);
const AUDIO_MIN_CORR = Number(process.env.PARITY_AUDIO_MIN_CORR ?? 0.99);

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

async function loadBindings(flavor) {
	const pkgDir = flavor === "simd" ? path.join(DSP_ROOT, "pkg-simd") : path.join(DSP_ROOT, "pkg");
	const jsPath = path.join(pkgDir, "hackrf_dsp.js");
	const wasmPath = path.join(pkgDir, "hackrf_dsp_bg.wasm");
	const mod = await import(pathToFileURL(jsPath).href);
	const wasmBytes = await readFile(wasmPath);
	const wasm = await mod.default({ module_or_path: wasmBytes });
	return { Receiver: mod.Receiver, wasm };
}

function fillIq(iqA, iqB, len, seed) {
	let phase = (seed * 0.007123) % (Math.PI * 2);
	let lcg = (seed * 1664525 + 1013904223) >>> 0;
	for (let i = 0; i < len; i += 2) {
		lcg = (lcg * 1664525 + 1013904223) >>> 0;
		const noise = ((lcg >>> 24) - 128) * 0.15;
		const re = Math.sin(phase) * 92 + Math.sin(phase * 0.37) * 22 + noise;
		const im = Math.cos(phase) * 91 + Math.cos(phase * 0.41) * 20 - noise;
		const i8re = Math.max(-127, Math.min(127, re | 0));
		const i8im = Math.max(-127, Math.min(127, im | 0));
		iqA[i] = i8re;
		iqA[i + 1] = i8im;
		iqB[i] = i8re;
		iqB[i + 1] = i8im;
		phase += 0.0314159 + (seed % 13) * 1e-4;
	}
}

function compareSlice(a, b, len) {
	let max = 0;
	for (let i = 0; i < len; i += 1) {
		const d = Math.abs(a[i] - b[i]);
		if (d > max) max = d;
	}
	return max;
}

function compareAudioWithLag(a, b, len, maxLag) {
	if (len <= 0) {
		return { lag: 0, rmse: 0, maxAbs: 0, corr: 1, refRms: 0 };
	}
	let best = {
		lag: 0,
		rmse: Number.POSITIVE_INFINITY,
		maxAbs: Number.POSITIVE_INFINITY,
		corr: -1,
		refRms: 0,
	};
	for (let lag = -maxLag; lag <= maxLag; lag += 1) {
		let i0 = 0;
		let j0 = 0;
		if (lag > 0) {
			i0 = lag;
		} else if (lag < 0) {
			j0 = -lag;
		}
		const overlap = len - Math.max(i0, j0);
		if (overlap <= 0) continue;

		let sumSqErr = 0;
		let maxAbs = 0;
		let sumAA = 0;
		let sumBB = 0;
		let sumAB = 0;
		for (let k = 0; k < overlap; k += 1) {
			const av = a[i0 + k];
			const bv = b[j0 + k];
			const d = av - bv;
			const ad = Math.abs(d);
			if (ad > maxAbs) maxAbs = ad;
			sumSqErr += d * d;
			sumAA += av * av;
			sumBB += bv * bv;
			sumAB += av * bv;
		}
		const rmse = Math.sqrt(sumSqErr / overlap);
		const refRms = Math.sqrt(sumAA / overlap);
		const denom = Math.sqrt(sumAA * sumBB);
		const corr = denom > 0 ? sumAB / denom : 1;

		if (rmse < best.rmse) {
			best = { lag, rmse, maxAbs, corr, refRms };
		}
	}
	if (!Number.isFinite(best.rmse)) {
		return { lag: 0, rmse: 0, maxAbs: 0, corr: 1, refRms: 0 };
	}
	return best;
}

function makeReceiver(bindings, cfg) {
	const receiver = new bindings.Receiver(
		cfg.sampleRate,
		cfg.centerFreq,
		cfg.targetFreq,
		cfg.demodMode,
		cfg.outputSampleRate,
		cfg.fftSize,
		0,
		cfg.fftVisibleBins,
		cfg.ifMinHz,
		cfg.ifMaxHz,
		true,
	);
	receiver.alloc_io_buffers(IQ_CAP, AUDIO_CAP, cfg.fftSize);
	return receiver;
}

async function runCase(cfg, baseBindings, simdBindings) {
	const base = makeReceiver(baseBindings, cfg);
	const simd = makeReceiver(simdBindings, cfg);
	const iqBase = new Int8Array(baseBindings.wasm.memory.buffer, base.iq_input_ptr(), IQ_CAP);
	const iqSimd = new Int8Array(simdBindings.wasm.memory.buffer, simd.iq_input_ptr(), IQ_CAP);
	const audioBase = new Float32Array(
		baseBindings.wasm.memory.buffer,
		base.audio_output_ptr(),
		base.audio_output_capacity(),
	);
	const audioSimd = new Float32Array(
		simdBindings.wasm.memory.buffer,
		simd.audio_output_ptr(),
		simd.audio_output_capacity(),
	);
	const fftBase = new Float32Array(baseBindings.wasm.memory.buffer, base.fft_output_ptr(), cfg.fftSize);
	const fftSimd = new Float32Array(simdBindings.wasm.memory.buffer, simd.fft_output_ptr(), cfg.fftSize);

	const iqLens = [
		IQ_CAP,
		IQ_CAP - 2,
		131_074,
		65_538,
		4_098,
		258,
	];

	let fftVisible = cfg.fftVisibleBins;
	let worstAudio = {
		lag: 0,
		rmse: 0,
		maxAbs: 0,
		corr: 1,
		refRms: 0,
	};
	let maxFftDiff = 0;

	for (let i = 0; i < BLOCKS; i += 1) {
		if (i === Math.floor(BLOCKS / 4)) {
			base.set_target_freq(cfg.centerFreq, cfg.targetFreq + 123_456);
			simd.set_target_freq(cfg.centerFreq, cfg.targetFreq + 123_456);
		}
		if (i === Math.floor(BLOCKS / 2)) {
			base.set_if_band(cfg.ifMinHz, cfg.ifMaxHz * 0.8);
			simd.set_if_band(cfg.ifMinHz, cfg.ifMaxHz * 0.8);
		}
		if (i === Math.floor((BLOCKS * 3) / 4)) {
			base.set_fft_view(57, cfg.fftSize - 57);
			simd.set_fft_view(57, cfg.fftSize - 57);
			fftVisible = cfg.fftSize - 57;
		}

		const iqLen = iqLens[i % iqLens.length];
		fillIq(iqBase, iqSimd, iqLen, i + 1);

		const audioLenBase = base.process_iq_len(iqLen);
		const audioLenSimd = simd.process_iq_len(iqLen);
		if (audioLenBase !== audioLenSimd) {
			throw new Error(
				`${cfg.name}: audio length mismatch block=${i} base=${audioLenBase} simd=${audioLenSimd}`,
			);
		}

		const audioStats = compareAudioWithLag(audioBase, audioSimd, audioLenBase, AUDIO_MAX_LAG);
		const fftDiff = compareSlice(fftBase, fftSimd, fftVisible);
		if (audioStats.rmse > worstAudio.rmse) worstAudio = audioStats;
		if (fftDiff > maxFftDiff) maxFftDiff = fftDiff;
		const rmseLimit = Math.max(5e-4, audioStats.refRms * AUDIO_RMSE_RATIO_MAX);
		if (audioStats.rmse > rmseLimit) {
			throw new Error(
				`${cfg.name}: audio rmse too large block=${i} rmse=${audioStats.rmse} limit=${rmseLimit} lag=${audioStats.lag} corr=${audioStats.corr}`,
			);
		}
		if (audioStats.corr < AUDIO_MIN_CORR) {
			throw new Error(
				`${cfg.name}: audio correlation too low block=${i} corr=${audioStats.corr} lag=${audioStats.lag}`,
			);
		}
		if (fftDiff > EPS_FFT) {
			throw new Error(
				`${cfg.name}: fft diff too large block=${i} diff=${fftDiff} eps=${EPS_FFT}`,
			);
		}
	}

	base.free_io_buffers();
	simd.free_io_buffers();
	base.free();
	simd.free();
	return { worstAudio, maxFftDiff };
}

const hasWasmArtifacts =
	existsSync(path.join(DSP_ROOT, "pkg", "hackrf_dsp.js")) &&
	existsSync(path.join(DSP_ROOT, "pkg", "hackrf_dsp_bg.wasm")) &&
	existsSync(path.join(DSP_ROOT, "pkg-simd", "hackrf_dsp.js")) &&
	existsSync(path.join(DSP_ROOT, "pkg-simd", "hackrf_dsp_bg.wasm"));

const parityTest =
	hasWasmArtifacts && supportsWasmSimd() ? it : it.skip;

describe("wasm simd parity", () => {
	parityTest(
		"base/simd の出力が許容範囲で一致する",
		async () => {
			const base = await loadBindings("base");
			const simd = await loadBindings("simd");
			const cases = [
				{
					name: "AM-2M",
					demodMode: "AM",
					sampleRate: 2_000_000,
					outputSampleRate: 48_000,
					fftSize: 1024,
					fftVisibleBins: 1024,
					centerFreq: 100_000_000,
					targetFreq: 100_000_000,
					ifMinHz: 0,
					ifMaxHz: 4_500,
				},
				{
					name: "AM-20M",
					demodMode: "AM",
					sampleRate: 20_000_000,
					outputSampleRate: 48_000,
					fftSize: 1024,
					fftVisibleBins: 1024,
					centerFreq: 100_000_000,
					targetFreq: 99_500_000,
					ifMinHz: 0,
					ifMaxHz: 4_500,
				},
				{
					name: "FM-2M",
					demodMode: "FM",
					sampleRate: 2_000_000,
					outputSampleRate: 48_000,
					fftSize: 1024,
					fftVisibleBins: 1024,
					centerFreq: 100_000_000,
					targetFreq: 100_120_000,
					ifMinHz: 0,
					ifMaxHz: 100_000,
				},
				{
					name: "FM-20M",
					demodMode: "FM",
					sampleRate: 20_000_000,
					outputSampleRate: 48_000,
					fftSize: 1024,
					fftVisibleBins: 1024,
					centerFreq: 100_000_000,
					targetFreq: 99_200_000,
					ifMinHz: 0,
					ifMaxHz: 100_000,
				},
			];

			for (const cfg of cases) {
				const result = await runCase(cfg, base, simd);
				expect(result.maxFftDiff, `${cfg.name} fft`).toBeLessThanOrEqual(EPS_FFT);
				expect(result.worstAudio.corr, `${cfg.name} audio corr`).toBeGreaterThanOrEqual(AUDIO_MIN_CORR);
			}
		},
		120_000,
	);
});
