/*
Copyright (c) 2019, cho45 <cho45@lowreal.net>

All rights reserved.

Redistribution and use in source and binary forms, with or without modification, are permitted provided that the following conditions are met:
    Redistributions of source code must retain the above copyright notice, this list of conditions and the following disclaimer.
    Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the following disclaimer in the
    documentation and/or other materials provided with the distribution.
    Neither the name of Great Scott Gadgets nor the names of its contributors may be used to endorse or promote products derived from this software
    without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO,
THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED.
IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
(INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION)
HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
*/

export function convertDecibelToRGB (dB) {
	var r = 0, g = 0, b = 0;
	var p = (dB + 48) / 48;

	switch (true) {
	case p > 5.0/6.0:
		// yellow -> red
		p = (p - (5 / 6.0)) / (1 / 6.0);
		r = 255;
		g = 255 * p;
		b = 255 * p;
		break;
	case p > 4.0/6.0:
		// yellow -> red
		p = (p - (4 / 6.0)) / (1 / 6.0);
		r = 255;
		g = 255 * (1 - p);
		b = 0;
		break;
	case p > 3.0/6.0:
		// green -> yellow
		p = (p - (3 / 6.0)) / (1 / 6.0);
		r = 255 * p;
		g = 255;
		b = 0;
		break;
	case p > 2.0/6.0:
		// light blue -> green
		p = (p - (2 / 6.0)) / (1 / 6.0);
		r = 0;
		g = 255;
		b = 255 * (1 - p);
		break;
	case p > 1.0/6.0:
		// blue -> light blue
		p = (p - (1 / 6.0)) / (1 / 6.0);
		r = 0;
		g = 255 * p;
		b = 255;
		break;
	case p > 0:
		// black -> blue
		p = p / (1 / 6.0);
		r = 0;
		g = 0;
		b = 255 * p;
	}

	return { r: r, g: g, b : b };
}


/**
 * WebGLを使った高速なウォーターフォール表示
 *
 * 【設計の意図】
 * 2つのテクスチャを循環させることで、テクスチャ全体のシフト（毎フレームの全転送）を回避する。
 * 1つのテクスチャだけでシフトを実現しようとすると、bandSize × historySize の転送が毎フレーム必要になるが、
 * この方式では常に bandSize × 1 行分の転送だけで済む。
 *
 * 【テクスチャの役割】
 * - textures[0]: 現在の書き込み先（新しいデータを _current 行目から順に埋めていく）
 * - textures[1]: 前回使用していたテクスチャ（古いデータの残りを表示する）
 *
 * 【シェーダーのロジック】
 * uOffsetY（現在の書き込み位置）を境界に2つのテクスチャを継ぎ目なく表示:
 * - 上半分 (screen.y >= uOffsetY): textures[1] を表示（古いデータを下からスクロール）
 * - 下半分 (screen.y < uOffsetY):  textures[0] を表示（新しいデータを下から積み上げ）
 *
 * 【循環のタイミング】
 * _current が historySize に達したら、textures[0] が「満杯」なので：
 * 1. textures 配列をローテート（[0,1] → [1,0]）→ 満杯のテクスチャが textures[1] になる
 * 2. _current を 0 にリセット
 * 3. 新しい textures[0] の先頭から書き込みを再開
 */
export class WaterfallGL {
	constructor(canvas, bandSize, historySize) {
		this.bandSize = bandSize;
		this.historySize = historySize;
		this.canvas = canvas;
		this.data = new Uint8Array(this.bandSize * 4);
		this.initWebGL();
	}

	initWebGL() {
		this._current = 0;

		this.canvas.width  = this.bandSize;
		this.canvas.height = this.historySize;

		try {
			this.gl = this.canvas.getContext("webgl") || this.canvas.getContext("experimental-webgl");
		} catch (e) {
		}

		if (!this.gl) {
			alert("Unable to initialize WebGL. Your browser may not support it.");
			return;
		}

		const gl = this.gl;

		gl.disable(gl.DEPTH_TEST);
		gl.disable(gl.CULL_FACE);
		gl.disable(gl.BLEND);

		gl.viewport(0, 0, this.canvas.width, this.canvas.height);
		gl.clearColor(0.0, 0.0, 0.0, 1.0);
		gl.clear(gl.COLOR_BUFFER_BIT);

		const fragmentShader = gl.createShader(gl.FRAGMENT_SHADER);
		gl.shaderSource(fragmentShader, `
			// uTexture0: 現在の書き込みテクスチャ（新しいデータ）
			// uTexture1: 前回のテクスチャ（古いデータの残り）
			// uViewCoords: ビューポートサイズ (width, height)
			// uOffsetY: 現在の書き込み位置（0 〜 historySize-1）
			uniform sampler2D uTexture0;
			uniform sampler2D uTexture1;
			uniform highp vec2 uViewCoords;
			uniform highp float uOffsetY;

			void main(void) {
				highp vec4 screen = gl_FragCoord;
				if (screen.y >= uOffsetY) {
					// 上半分: 古いデータ（uTexture1）を表示
					// 下から uOffsetY 分だけスクロールして表示
					screen.y = uViewCoords.y + uOffsetY - screen.y;
					highp vec2 screenTexCoord = screen.xy / uViewCoords.xy;
					gl_FragColor = texture2D(uTexture1, screenTexCoord);
				} else {
					// 下半分: 新しいデータ（uTexture0）を表示
					// 下から順に 0, 1, 2... と積み上がっているので反転
					screen.y = uViewCoords.y - screen.y + uOffsetY;
					highp vec2 screenTexCoord = screen.xy / uViewCoords.xy;
					gl_FragColor = texture2D(uTexture0, screenTexCoord);
				}
			}
		`);
		gl.compileShader(fragmentShader);
		if (!gl.getShaderParameter(fragmentShader, gl.COMPILE_STATUS)) {
			alert("An error occurred compiling the shaders: " + gl.getShaderInfoLog(fragmentShader));
			return;
		}

		const vertexShader = gl.createShader(gl.VERTEX_SHADER);
		gl.shaderSource(vertexShader, `
			attribute vec3 aVertexPosition;

			void main(void) {
				gl_Position = vec4(aVertexPosition, 1.0);
			}
		`);
		gl.compileShader(vertexShader);
		if (!gl.getShaderParameter(vertexShader, gl.COMPILE_STATUS)) {
			alert("An error occurred compiling the shaders: " + gl.getShaderInfoLog(vertexShader));
			return;
		}

		this.shaderProgram = gl.createProgram();
		gl.attachShader(this.shaderProgram, vertexShader);
		gl.attachShader(this.shaderProgram, fragmentShader);
		gl.linkProgram(this.shaderProgram);

		if (!gl.getProgramParameter(this.shaderProgram, gl.LINK_STATUS)) {
			alert("Unable to initialize the shader program.");
		}

		gl.useProgram(this.shaderProgram);

		this.vertexPositionAttribute = gl.getAttribLocation(this.shaderProgram, "aVertexPosition");
		gl.enableVertexAttribArray(this.vertexPositionAttribute);

		this.vertices1 = gl.createBuffer();
		gl.bindBuffer(gl.ARRAY_BUFFER, this.vertices1);
		gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([
			1.0,  1.0,  0.0,
			-1.0, 1.0,  0.0,
			1.0,  -1.0, 0.0,
			-1.0, -1.0, 0.0
		]), gl.STATIC_DRAW);

		// texture sources
		this.textures = [gl.createTexture(), gl.createTexture()];

		// 2の累乗サイズに切り上げてテクスチャを初期化
		// （古いWebGLの制約への対応。NPOT非対応環境でも動作させる）
		this.canvas.width  = Math.pow(2, Math.ceil(Math.log2(this.bandSize)));
		console.log({glInit: this.canvas.width});
		this.canvas.height = this.historySize;
		console.log(this.canvas.width, this.bandSize);

		for (var i = 0, it; (it = this.textures[i]); i++) {
			gl.bindTexture(gl.TEXTURE_2D, it);
			gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, true);
			gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.NONE);
			gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, this.canvas);
			gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
			gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
			gl.bindTexture(gl.TEXTURE_2D, null);
		}

		gl.uniform2f(gl.getUniformLocation(this.shaderProgram, 'uViewCoords'), this.canvas.width, this.canvas.height);

		gl.bindBuffer(gl.ARRAY_BUFFER, this.vertices1);
		gl.vertexAttribPointer(this.vertexPositionAttribute, 3, gl.FLOAT, false, 0, 0);

		gl.activeTexture(gl.TEXTURE1);
		gl.bindTexture(gl.TEXTURE_2D, this.textures[1]);
		gl.uniform1i(gl.getUniformLocation(this.shaderProgram, "uTexture1"), 1);

		gl.activeTexture(gl.TEXTURE0);
		gl.bindTexture(gl.TEXTURE_2D, this.textures[0]);
		gl.uniform1i(gl.getUniformLocation(this.shaderProgram, "uTexture0"), 0);

		gl.bindTexture(gl.TEXTURE_2D, this.textures[0]);

		this.canvas.width  = this.bandSize;
		this.canvas.height = this.historySize;

		this.render();
	}

	render() {
		const gl = this.gl;

		gl.uniform1f(gl.getUniformLocation(this.shaderProgram, 'uOffsetY'), this._current);

		gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
	}

	renderLine(array) {
		const gl = this.gl;
		const data = this.data;

		for (let i = 0, len = this.bandSize; i < len; i++) {
			const n = i * 4;
			const rgb = convertDecibelToRGB(array[i]);

			data[n + 0] = rgb.r;
			data[n + 1] = rgb.g;
			data[n + 2] = rgb.b;
			data[n + 3] = 255;
		}

		const xoffset = 0, yoffset = this._current, width = this.bandSize, height = 1;
		gl.texSubImage2D(gl.TEXTURE_2D, 0, xoffset, yoffset, width, height, gl.RGBA, gl.UNSIGNED_BYTE, data);

		this._current++;

		if (this._current >= this.historySize) {
			// テクスチャが満杯になったら循環させる
			// [A, B] → [B, A] となり、A（満杯）が「古いデータ」として使われる
			this._current = 0;
			this.textures.push(this.textures.shift());

			gl.activeTexture(gl.TEXTURE1);
			gl.bindTexture(gl.TEXTURE_2D, this.textures[1]);
			gl.uniform1i(gl.getUniformLocation(this.shaderProgram, "uTexture1"), 1);

			gl.activeTexture(gl.TEXTURE0);
			gl.bindTexture(gl.TEXTURE_2D, this.textures[0]);
			gl.uniform1i(gl.getUniformLocation(this.shaderProgram, "uTexture0"), 0);

		}

		this.render();
	}
}

export class Waterfall {
	constructor(canvas, bandSize, historySize) {
		this.bandSize = bandSize;
		this.historySize = historySize;
		this.canvas = canvas;
		this.data = new Uint8Array(this.bandSize * 4);
		this.canvas.width  = this.bandSize;
		this.canvas.height = this.historySize;
		this.ctx = this.canvas.getContext('2d');
	}

	renderLine(array) {
		const { canvas, ctx } = this;

		// shift data to up
		ctx.drawImage(
			canvas,
			0, 1, canvas.width, canvas.height - 1,
			0, 0, canvas.width, canvas.height - 1
		);

		var imageData = ctx.getImageData(0, canvas.height, canvas.width, 1);
		var data = imageData.data; // rgba

		for (var i = 0, len = canvas.width; i < len; i++) {
			var n = i * 4;
			var rgb = convertDecibelToRGB(array[i]);

			data[n + 0] = rgb.r;
			data[n + 1] = rgb.g;
			data[n + 2] = rgb.b;
			data[n + 3] = 255;
		}

		ctx.putImageData(imageData, 0, canvas.height-1);
	}
}
