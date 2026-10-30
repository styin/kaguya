// AudioWorkletProcessor for mic capture.
// Runs in the audio thread — no DOM access, no imports.
// Registered by capture.ts via addModule().

const CAPTURE_PROCESSOR = `
class CaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this._buffer = new Float32Array(0);
  }

  process(inputs) {
    const input = inputs[0]?.[0];
    if (!input) return true;

    // Accumulate samples
    const merged = new Float32Array(this._buffer.length + input.length);
    merged.set(this._buffer);
    merged.set(input, this._buffer.length);
    this._buffer = merged;

    // Flush 20ms chunks (320 samples at 16kHz)
    while (this._buffer.length >= 320) {
      const chunk = this._buffer.slice(0, 320);
      this._buffer = this._buffer.slice(320);

      // float32 → int16
      const pcm = new Int16Array(320);
      for (let i = 0; i < 320; i++) {
        const s = Math.max(-1, Math.min(1, chunk[i]));
        pcm[i] = s < 0 ? s * 0x8000 : s * 0x7FFF;
      }
      this.port.postMessage(pcm.buffer, [pcm.buffer]);
    }
    return true;
  }
}
registerProcessor("capture-processor", CaptureProcessor);
`;

export function createWorkletBlobUrl(): string {
  const blob = new Blob([CAPTURE_PROCESSOR], { type: "application/javascript" });
  return URL.createObjectURL(blob);
}
