import assert from 'assert';
import { FFT } from './node/hackrf_web.js';

async function test() {
	// WASM モジュールは自動的にロードされる
	console.log('✓ WASM module loaded');

	// FFT インスタンス作成（JS 側の Float32Array を渡す）
	const n = 8;
	const window = new Float32Array(n).fill(1.0);
	const fft = new FFT(n, window);
	console.log('✓ FFT instance created');

	// DC入力（JS 側の Int8Array）
	const input = new Int8Array(n * 2);
	for (let i = 0; i < n; i++) {
		input[i * 2] = 64;     // 実部 = 0.5 (64/128)
		input[i * 2 + 1] = 0;  // 虚部 = 0
	}
	console.log('✓ Input array created (DC signal)');

	// 出力バッファ（JS 側の Float32Array）
	const output = new Float32Array(n);

	// JS ↔ WASM バインディングを通して FFT 実行
	fft.fft(input, output);
	console.log('✓ FFT executed via JS binding');

	// 結果検証
	assert.strictEqual(output.length, n, 'Output length should match FFT size');

	// DC成分（index 4 = n/2）の値を確認
	const dcIndex = n / 2;
	console.log('DC component (index ' + dcIndex + '):', output[dcIndex]);
	console.log('First component (index 0):', output[0]);

	// DC成分が他の周波数より大きいことを確認
	assert.ok(output[dcIndex] > output[0], 'DC component should be greater than other frequencies');

	// 出力値を dB 単位なので妥当な範囲内にあるか確認
	assert.ok(output[dcIndex] < 0, 'DC component should be negative (dB scale)');
	assert.ok(output[dcIndex] > -100, 'DC component should be greater than -100 dB');

	console.log('Output values:', Array.from(output));
	console.log('✓ DC component validation passed');

	console.log('\n✅ All tests passed!');
}

test().catch(err => {
	console.error('❌ Test failed:', err);
	process.exit(1);
});
