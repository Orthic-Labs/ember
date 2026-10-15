import type { ControlSpec } from '../types';

// Mirrors `ControlSpec::position` / `ControlSpec::value_at` in crates/develop/src/controls.rs: the Kelvin
// temperature follows Lightroom's track, 2000-7000 K evenly over the left half and a logarithmic right half up
// to the maximum; every other control is linear.
const KELVIN_CENTRE = 7000;
const KELVIN_KNEE = 1000;
/** Range-input resolution for a non-linear track. */
export const TRACK_STEPS = 1000;

function kelvinScale(spec: ControlSpec): { centre: number; right: number } | null {
  if (spec.id !== 'wb.temp' || !(spec.min < KELVIN_CENTRE && KELVIN_CENTRE < spec.max)) return null;
  return { centre: KELVIN_CENTRE, right: Math.log(1 + (spec.max - KELVIN_CENTRE) / KELVIN_KNEE) };
}

const clamp = (spec: ControlSpec, v: number) => Math.max(spec.min, Math.min(spec.max, v));

/** Where `v` sits on the track, 0 (left) to 1 (right). */
export function trackPosition(spec: ControlSpec, v: number): number {
  const value = clamp(spec, v);
  const k = kelvinScale(spec);
  const p = k === null
    ? (value - spec.min) / Math.max(spec.max - spec.min, 1e-9)
    : value > k.centre
      ? 0.5 + 0.5 * Math.log(1 + (value - k.centre) / KELVIN_KNEE) / k.right
      : 0.5 * (value - spec.min) / (k.centre - spec.min);
  return Number.isFinite(p) ? Math.max(0, Math.min(1, p)) : 0;
}

/** The value at track position `p` (0..1), snapped to the control's step. */
export function trackValue(spec: ControlSpec, p: number): number {
  const t = Number.isFinite(p) ? Math.max(0, Math.min(1, p)) : 0;
  const k = kelvinScale(spec);
  const v = k === null
    ? spec.min + t * (spec.max - spec.min)
    : t >= 1
      ? spec.max
      : t > 0.5
        ? k.centre + KELVIN_KNEE * (Math.exp((2 * t - 1) * k.right) - 1)
        : spec.min + 2 * t * (k.centre - spec.min);
  const snapped = spec.step > 0 ? Math.round(v / spec.step) * spec.step : v;
  return clamp(spec, snapped);
}

/** True when the control's range input works in track positions rather than values. */
export const nonLinearTrack = (spec: ControlSpec) => kelvinScale(spec) !== null;

/** Props for the range input showing `value`. */
export function rangeProps(spec: ControlSpec, value: number): { min: number; max: number; step: number; value: number } {
  return nonLinearTrack(spec)
    ? { min: 0, max: TRACK_STEPS, step: 1, value: Math.round(trackPosition(spec, value) * TRACK_STEPS) }
    : { min: spec.min, max: spec.max, step: spec.step, value };
}

/** The control value a range input's raw value stands for. */
export const fromRange = (spec: ControlSpec, raw: number) => (nonLinearTrack(spec) ? trackValue(spec, raw / TRACK_STEPS) : raw);
