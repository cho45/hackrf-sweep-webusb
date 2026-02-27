#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const RADIO_ROOT = path.resolve(HERE, "..");
const DSP_ROOT = path.join(RADIO_ROOT, "hackrf-dsp");

const flavor = String(process.env.WASM_FLAVOR ?? "both").toLowerCase();
const noOptRaw = String(process.env.WASM_NO_OPT ?? "1").toLowerCase();
const noOpt = noOptRaw === "1" || noOptRaw === "true" || noOptRaw === "on";
const mode = String(process.env.WASM_MODE ?? "no-install");

const pickFlavors = () => {
	if (flavor === "base") return ["base"];
	if (flavor === "simd") return ["simd"];
	if (flavor === "both") return ["base", "simd"];
	throw new Error(`invalid WASM_FLAVOR=${flavor} (expected: base|simd|both)`);
};

const run = (cmd, args, extraEnv = {}) => {
	const result = spawnSync(cmd, args, {
		cwd: DSP_ROOT,
		stdio: "inherit",
		env: {
			...process.env,
			...extraEnv,
		},
	});
	if (typeof result.status === "number" && result.status !== 0) {
		process.exit(result.status);
	}
	if (result.error) {
		throw result.error;
	}
};

const buildFlavor = (f) => {
	const args = ["build", "--target", "web", "--out-dir", f === "base" ? "pkg" : "pkg-simd", "--mode", mode];
	if (noOpt) {
		args.push("--no-opt");
	}
	if (f === "simd") {
		const rustflags = [process.env.RUSTFLAGS, "-C target-feature=+simd128"].filter(Boolean).join(" ");
		run("wasm-pack", args, { RUSTFLAGS: rustflags });
		return;
	}
	run("wasm-pack", args);
};

console.log(`[build-wasm] flavor=${flavor} no_opt=${noOpt} mode=${mode}`);
for (const f of pickFlavors()) {
	console.log(`[build-wasm] building ${f}`);
	buildFlavor(f);
}
