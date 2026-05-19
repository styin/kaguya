import { useEffect, useRef } from "react";

/**
 * Drives N bars from an AnalyserNode via requestAnimationFrame. Writes
 * directly to each bar's `transform: scaleY(...)` — no React re-render
 * per frame.
 *
 * Time-domain sampling (not frequency). `getByteTimeDomainData` returns
 * the raw waveform as bytes centered at 128 — peak deviation from 128
 * is amplitude. We use this instead of `getByteFrequencyData` because
 * the latter normalizes against `minDecibels/maxDecibels` (defaults
 * −100/−30 dB) and reads near-zero for soft signals, which made the
 * bars idle even while TTS played.
 *
 * `getAnalyser` is polled rather than passed once so the meter survives
 * mic/tts start/stop without remounting.
 */
export function useMeterBars(
  getAnalyser: () => AnalyserNode | null,
  barCount: number,
): React.RefObject<HTMLDivElement | null>[] {
  const refs = useRef<React.RefObject<HTMLDivElement | null>[]>(
    Array.from({ length: barCount }, () => ({ current: null })),
  );
  // Per-bar smoothed value so frames don't jitter; pure visual easing.
  const smoothed = useRef<Float32Array>(new Float32Array(barCount));

  useEffect(() => {
    let raf = 0;
    // fftSize=256 → 256 time-domain samples per read (~16ms at 16kHz).
    const SAMPLE_LEN = 256;
    const buf = new Uint8Array(SAMPLE_LEN);

    function tick() {
      const a = getAnalyser();
      if (a) {
        if (a.fftSize !== SAMPLE_LEN) a.fftSize = SAMPLE_LEN;
        a.getByteTimeDomainData(buf);
        const chunk = Math.max(1, Math.floor(SAMPLE_LEN / barCount));
        for (let i = 0; i < barCount; i++) {
          let peak = 0;
          const start = i * chunk;
          const end = Math.min(SAMPLE_LEN, start + chunk);
          for (let j = start; j < end; j++) {
            const dev = Math.abs(buf[j] - 128);
            if (dev > peak) peak = dev;
          }
          // peak ∈ [0, 128]. Scale to [0, 1] with mild boost so quiet
          // voice still registers visually; clamp at 1.
          const v = Math.min(1, (peak / 128) * 1.8);
          // Asymmetric easing: rise fast, fall slow — matches a VU
          // meter and stops the bars from looking flat between syllables.
          const prev = smoothed.current[i];
          smoothed.current[i] = v > prev ? v : prev * 0.85 + v * 0.15;
          const node = refs.current[i]?.current;
          if (node) node.style.transform = `scaleY(${0.05 + smoothed.current[i] * 0.95})`;
        }
      } else {
        for (let i = 0; i < barCount; i++) {
          smoothed.current[i] = 0;
          const node = refs.current[i]?.current;
          if (node) node.style.transform = `scaleY(0.05)`;
        }
      }
      raf = requestAnimationFrame(tick);
    }

    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [getAnalyser, barCount]);

  return refs.current;
}
