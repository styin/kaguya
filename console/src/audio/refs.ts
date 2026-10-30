// Live mic meter reference. Web Audio objects stay outside React state;
// capture.ts sets it and AudioStrip reads it in an animation-frame loop.
export const audioRefs: { mic: AnalyserNode | null } = { mic: null };
