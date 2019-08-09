# HackRF Sweep WebUSB

This is spectrum analyzer implementation in JavaScript with WebUSB for <a href="https://greatscottgadgets.com/hackrf/">HackRF</a>.

<img src="./doc/dfe049fbba1adc9c9e0e21e2449f72cd.gif">

# Usage

There are no requirements except a browser supporting WebUSB (available by default with Google Chrome currently)

1. Access to https://cho45.stfuawsc.com/hackrf-webusb/ .
2. Connect your HackRF to USB port.
3. Click [CONNECT] and select the device.
4. Set range for analysis.
5. Click [START].
6. Adjast gains.

# Implementation

1. Communication with HackRF device with <strong>WebUSB</strong>.
2. Run FFT with <strong>WebAssembly</strong> which is written in Rust (using <a href="https://github.com/awelkie/RustFFT">RustFFT</a>)
3. Show results with <strong>WebGL</strong> waterfall implementation.
