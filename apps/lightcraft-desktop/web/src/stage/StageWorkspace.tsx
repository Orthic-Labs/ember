import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { PointerEvent } from "react";
import { Button, Card, IconButton, Popover, Select, Slider } from "@rightkit/react-shell/react";
import { PhotoPreview } from "../preview/PhotoPreview";
import Filmstrip from "../library/Filmstrip";
import { useDesktop } from "../desktop";
import type { DesktopSnapshot, PhotoSummary, UiState, ViewMode } from "../types";
import { longToNormalized, normalizedPhotoPoint, normalizedToLong, normalizedToOutputPoint, orientedAspectFor, outputAspectFor, radialHandleAt, radialHandles, radialShapeFromDrag, rotateNormalizedPoint } from "./maskGeometry";
import type { DevelopShape, Point } from "./maskGeometry";
import "./StageWorkspace.css";

type Box = { x0: number; y0: number; x1: number; y1: number };
type MaskShapeView = { shape: DevelopShape; id: number; component: number; visible: boolean };

export { longToNormalized, normalizedPhotoPoint, normalizedToLong, normalizedToOutputPoint, orientedAspectFor, outputAspectFor, radialHandleAt, radialHandles, radialShapeFromDrag, rotateNormalizedPoint } from "./maskGeometry";

const views: Array<{ id: ViewMode; label: string; key: string }> = [
  { id: "detail", label: "Detail", key: "D" },
  { id: "compare", label: "Compare", key: "C" },
  { id: "survey", label: "Survey", key: "N" },
  { id: "reference", label: "Reference", key: "R" },
  { id: "people", label: "People", key: "P" },
];

const maskTools = [
  ["brush", "Brush"],
  ["linear", "Linear Gradient"],
  ["radial", "Radial Gradient"],
  ["colorRange", "Color Range"],
  ["luminanceRange", "Luminance Range"],
  ["object", "Object (SAM)"],
  ["sky", "Sky"],
  ["subject", "Subject"],
  ["background", "Background"],
] as const;

function record(value: unknown): DevelopShape {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as DevelopShape) : {};
}

function number(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function point(value: unknown): Point | null {
  if (Array.isArray(value) && value.length >= 2) return { x: number(value[0]), y: number(value[1]) };
  if (value && typeof value === "object") {
    const p = value as Record<string, unknown>;
    if (typeof p.x === "number" && typeof p.y === "number") return { x: p.x, y: p.y };
  }
  return null;
}

function box(value: unknown): Box | null {
  if (Array.isArray(value) && value.length >= 4) return { x0: number(value[0]), y0: number(value[1]), x1: number(value[2]), y1: number(value[3]) };
  if (value && typeof value === "object") {
    const p = value as Record<string, unknown>;
    if (["x0", "y0", "x1", "y1"].every((k) => typeof p[k] === "number")) return { x0: number(p.x0), y0: number(p.y0), x1: number(p.x1), y1: number(p.y1) };
  }
  return null;
}

function cropOf(develop: DevelopShape): { rect: Box; angle: number } {
  const crop = record(develop.crop);
  const geometry = record(crop.geometry);
  const rect = box(geometry.rect ?? crop.rect) ?? { x0: 0, y0: 0, x1: 1, y1: 1 };
  return { rect, angle: number(geometry.angle ?? crop.angle) };
}

const rotatePoint = rotateNormalizedPoint;

function invertPhotoPoint(event: PointerEvent<HTMLElement>, element: HTMLElement, develop: DevelopShape, sourceAspect: number, zoom: number, pan: Point): Point {
  return normalizedPhotoPoint(event.clientX, event.clientY, element.getBoundingClientRect(), develop, sourceAspect, zoom, pan);
}

function cropHandleAt(value: Point, rect: Box): number | null {
  const handles = [{ x: rect.x0, y: rect.y0 }, { x: rect.x1, y: rect.y0 }, { x: rect.x1, y: rect.y1 }, { x: rect.x0, y: rect.y1 }, { x: (rect.x0 + rect.x1) / 2, y: rect.y0 }, { x: rect.x1, y: (rect.y0 + rect.y1) / 2 }, { x: (rect.x0 + rect.x1) / 2, y: rect.y1 }, { x: rect.x0, y: (rect.y0 + rect.y1) / 2 }];
  let nearest: number | null = null;
  let distance = 0.045;
  handles.forEach((handle, index) => {
    const next = Math.hypot(value.x - handle.x, value.y - handle.y);
    if (next < distance) { distance = next; nearest = index; }
  });
  return nearest;
}

function photosIn(snapshot: DesktopSnapshot | null): PhotoSummary[] {
  const source = record(snapshot?.source);
  const cached = (snapshot as DesktopSnapshot & { photos?: PhotoSummary[] } | null)?.photos ?? (Array.isArray(source.photos) ? source.photos as PhotoSummary[] : undefined);
  return cached ?? [];
}

function photoForId(snapshot: DesktopSnapshot | null, id: number | null): PhotoSummary | null {
  return id == null ? null : photosIn(snapshot).find((photo) => photo.id === id) ?? null;
}

function inspectedSummary(value: unknown, fallbackId: number): PhotoSummary | null {
  const source = record(value);
  const meta = record(source.meta);
  const id = number(source.id, fallbackId);
  const width = number(source.width ?? source.w);
  const height = number(source.height ?? source.h);
  if (!Number.isFinite(id) || width <= 0 || height <= 0) return null;
  const flag = source.flag === "pick" || source.flag === "reject" ? source.flag : "none";
  const label = typeof source.label === "string" ? source.label : null;
  return {
    id,
    fileName: typeof source.file_name === "string" ? source.file_name : typeof source.fileName === "string" ? source.fileName : `Photo ${id}`,
    format: typeof source.format === "string" ? source.format : "",
    kind: typeof source.kind === "string" ? source.kind : "image",
    w: width,
    h: height,
    captured: typeof source.captured === "string" ? source.captured : null,
    imported: typeof source.imported === "string" ? source.imported : undefined,
    rating: number(source.rating),
    flag,
    label,
    edited: typeof source.edited === "string" || source.edited === true,
    title: typeof meta.title === "string" ? meta.title : "",
    keywords: Array.isArray(meta.keywords) ? meta.keywords.filter((item): item is string => typeof item === "string") : [],
    camera: typeof meta.camera === "string" ? meta.camera : "",
    lens: typeof meta.lens === "string" ? meta.lens : undefined,
    shutter: typeof meta.shutter === "string" ? meta.shutter : undefined,
    aperture: typeof meta.aperture === "number" && Number.isFinite(meta.aperture) ? meta.aperture : null,
    iso: typeof meta.iso === "number" && Number.isFinite(meta.iso) ? meta.iso : null,
    focalMm: typeof meta.focal_mm === "number" && Number.isFinite(meta.focal_mm) ? meta.focal_mm : null,
    deleted: source.deleted === true,
    copyOf: typeof source.copy_of === "number" ? source.copy_of : typeof source.copyOf === "number" ? source.copyOf : null,
    copyName: typeof source.copy_name === "string" ? source.copy_name : typeof source.copyName === "string" ? source.copyName : undefined,
    previewOnly: source.preview_only === true || source.previewOnly === true,
  };
}

function useInspectedPhotos(ids: number[]): Map<number, PhotoSummary> {
  const { run } = useDesktop();
  const key = ids.join(",");
  const [photos, setPhotos] = useState<Map<number, PhotoSummary>>(() => new Map());
  useEffect(() => {
    let live = true;
    setPhotos(new Map());
    if (!ids.length) return () => { live = false; };
    void Promise.all(ids.map(async (id) => {
      try {
        return [id, inspectedSummary(await run("photo.inspect", { id }), id)] as const;
      } catch {
        return [id, null] as const;
      }
    })).then((entries) => {
      if (!live) return;
      setPhotos(new Map(entries.flatMap(([id, photo]) => photo ? [[id, photo] as const] : [])));
    });
    return () => { live = false; };
  }, [key, run]);
  return photos;
}

function previewDimensions(photo: PhotoSummary | null, previewEdge: number): { width: number; height: number } {
  const edge = Math.max(256, Math.min(8192, Math.round(Number.isFinite(previewEdge) ? previewEdge : 2560)));
  const width = photo && Number.isFinite(photo.w) && photo.w > 0 ? photo.w : 4;
  const height = photo && Number.isFinite(photo.h) && photo.h > 0 ? photo.h : 3;
  const scale = edge / Math.max(width, height);
  return { width: Math.max(1, Math.round(width * scale)), height: Math.max(1, Math.round(height * scale)) };
}

function previewSlot(view: ViewMode, index = 0): string {
  return `${view}-${index === 0 ? "main" : `candidate-${index}`}`;
}

function titleFor(photo: PhotoSummary | null): string {
  return photo?.title || photo?.fileName || "No photo selected";
}

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return target.isContentEditable
    || target.closest("[contenteditable='true']") !== null
    || ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName)
    || target.getAttribute("role") === "textbox";
}

function PreviewPane({
  photoId,
  photo,
  previewEdge,
  slot,
  viewGeneration,
  before,
  className,
  onHistogram,
}: {
  photoId: number | null;
  photo: PhotoSummary | null;
  previewEdge: number;
  slot: string;
  viewGeneration: number;
  before?: boolean;
  className?: string;
  onHistogram: (value: unknown) => void;
}) {
  if (photoId == null) return <div className={`stage-empty ${className ?? ""}`}>Select a photo to view its decoded preview</div>;
  // Metadata inspection is asynchronous; request real pixels immediately for a valid ID.
  // Native renderer preserves source aspect, while inspected metadata later sizes the request.
  const dimensions = photo ? previewDimensions(photo, previewEdge) : { width: previewEdge, height: previewEdge };
  return <PhotoPreview photoId={photoId} slot={slot} viewGeneration={viewGeneration} width={dimensions.width} height={dimensions.height} quality="full" before={before} className={className} onHistogram={onHistogram} />;
}

function StageImage({
  photoId,
  photo,
  previewEdge,
  slot,
  viewGeneration,
  before,
  develop,
  onHistogram,
  onPointerDown,
  onPointerMove,
  onPointerUp,
  onPointerCancel,
}: {
  photoId: number | null;
  photo: PhotoSummary | null;
  previewEdge: number;
  slot: string;
  viewGeneration: number;
  before?: boolean;
  develop: DevelopShape;
  onHistogram: (value: unknown) => void;
  onPointerDown?: (event: PointerEvent<HTMLElement>) => void;
  onPointerMove?: (event: PointerEvent<HTMLElement>) => void;
  onPointerUp?: (event: PointerEvent<HTMLElement>) => void;
  onPointerCancel?: (event: PointerEvent<HTMLElement>) => void;
}) {
  return (
    <div className="stage-image-wrap" onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={onPointerUp} onPointerCancel={onPointerCancel}>
      <div className="stage-image-transform">
        <PreviewPane photoId={photoId} photo={photo} previewEdge={previewEdge} slot={slot} viewGeneration={viewGeneration} before={before} className="stage-preview" onHistogram={onHistogram} />
      </div>
    </div>
  );
}

function CropOverlay({ develop, guide = "thirds" }: { develop: DevelopShape; guide?: string }) {
  const { rect } = cropOf(develop);
  return <div className="crop-overlay" style={{ left: `${rect.x0 * 100}%`, top: `${rect.y0 * 100}%`, width: `${Math.max(0, rect.x1 - rect.x0) * 100}%`, height: `${Math.max(0, rect.y1 - rect.y0) * 100}%`, transform: `rotate(${cropOf(develop).angle}deg)` }}><i className="crop-handle crop-nw" /><i className="crop-handle crop-ne" /><i className="crop-handle crop-sw" /><i className="crop-handle crop-se" />{guide !== "off" ? <><i className="crop-guide crop-guide-v1" /><i className="crop-guide crop-guide-v2" /><i className="crop-guide crop-guide-h1" /><i className="crop-guide crop-guide-h2" /></> : null}</div>;
}

function maskShapes(develop: DevelopShape, mode: string, active: number): MaskShapeView[] {
  if (mode === "off") return [];
  const masks = Array.isArray(develop.masks) ? develop.masks : [];
  return masks.flatMap((mask) => {
    const m = record(mask);
    const id = number(m.id, -1);
    if (m.visible === false || (mode === "selected" && id !== active)) return [];
    const components = Array.isArray(m.components) ? m.components : [];
    return components.map((component, componentIndex) => ({
      shape: record(record(component).shape),
      id,
      component: componentIndex,
      visible: true,
    }));
  });
}

function shapePoint(value: unknown): Point | null {
  return point(value);
}

function shapePins(shape: DevelopShape): Point[] {
  const kind = String(shape.kind ?? "");
  if (kind === "radial") return [shapePoint(shape.center)].filter((value): value is Point => value !== null);
  if (kind === "linear") return [shapePoint(shape.start), shapePoint(shape.end)].filter((value): value is Point => value !== null);
  if (kind === "object") {
    return [
      ...(Array.isArray(shape.hint) ? shape.hint : []),
      ...(Array.isArray(shape.exclude) ? shape.exclude : []),
    ].map(shapePoint).filter((value): value is Point => value !== null);
  }
  if (kind === "brush") {
    const strokes = Array.isArray(shape.strokes) ? shape.strokes : [];
    return strokes.flatMap((stroke) => {
      const raw = record(stroke).points;
      const points: unknown[] = Array.isArray(raw) ? raw : [];
      return [points[0], points[points.length - 1]].map(shapePoint).filter((value): value is Point => value !== null);
    });
  }
  return [];
}

function svgPoint(value: Point, outputAspect: number): Point {
  return { x: value.x * outputAspect * 100, y: value.y * 100 };
}

function brushPath(shape: DevelopShape, develop: DevelopShape, sourceAspect: number, outputAspect: number): string | null {
  const strokes = Array.isArray(shape.strokes) ? shape.strokes : [];
  const path = strokes.map((stroke) => {
    const raw = record(stroke).points;
    const points: Point[] = (Array.isArray(raw) ? raw : []).map(shapePoint).filter((value): value is Point => value !== null);
    if (!points.length) return "";
    return `M ${points.map((value) => { const p = svgPoint(normalizedToOutputPoint(value, develop, sourceAspect), outputAspect); return `${p.x} ${p.y}`; }).join(" L ")}`;
  }).filter(Boolean).join(" ");
  return path || null;
}

function MaskOverlay({ develop, activeMask, mode, pins, sourceAspect, zoom, pan, brushPreview }: { develop: DevelopShape; activeMask: number; mode: string; pins: boolean; sourceAspect: number; zoom: number; pan: Point; brushPreview?: Point[] }) {
  const visible = maskShapes(develop, mode, activeMask);
  const aspect = orientedAspectFor(develop, sourceAspect);
  const outputAspect = outputAspectFor(develop, sourceAspect);
  const project = (value: Point) => normalizedToOutputPoint(value, develop, sourceAspect);
  const toSvg = (value: Point) => svgPoint(value, outputAspect);
  return <div className="mask-overlay" aria-hidden="true">
    <svg className="mask-vector" viewBox={`0 0 ${outputAspect * 100} 100`} preserveAspectRatio="xMidYMid meet" style={{ transform: `translate(${pan.x * 100}%, ${pan.y * 100}%) scale(${zoom})` }}>
      {visible.map(({ shape, id, component }) => {
        const kind = String(shape.kind ?? "");
        if (kind === "brush") {
          const path = brushPath(shape, develop, sourceAspect, outputAspect);
          return path ? <path key={`${id}-${component}`} className="mask-brush-path" d={path} /> : null;
        }
        if (kind === "linear") {
          const start = shapePoint(shape.start);
          const end = shapePoint(shape.end);
          const a = start ? toSvg(project(start)) : null;
          const b = end ? toSvg(project(end)) : null;
          return a && b ? <line key={`${id}-${component}`} className="mask-linear-line" x1={a.x} y1={a.y} x2={b.x} y2={b.y} /> : null;
        }
        if (kind === "radial") {
          const center = shapePoint(shape.center);
          const rx = number(shape.rx);
          const ry = number(shape.ry);
          if (!center || !(rx > 0 && ry > 0)) return null;
          const angle = number(shape.angle) * Math.PI / 180;
          const centerOutput = toSvg(project(center));
          const u = { x: rx * Math.cos(angle), y: rx * Math.sin(angle) };
          const v = { x: -ry * Math.sin(angle), y: ry * Math.cos(angle) };
          const uPoint = toSvg(project(longToNormalized({ x: normalizedToLong(center, aspect).x + u.x, y: normalizedToLong(center, aspect).y + u.y }, aspect)));
          const vPoint = toSvg(project(longToNormalized({ x: normalizedToLong(center, aspect).x + v.x, y: normalizedToLong(center, aspect).y + v.y }, aspect)));
          const ux = uPoint.x - centerOutput.x;
          const uy = uPoint.y - centerOutput.y;
          const vx = vPoint.x - centerOutput.x;
          const vy = vPoint.y - centerOutput.y;
          const radiusX = Math.hypot(ux, uy);
          const radiusY = Math.hypot(vx, vy);
          const ellipseAngle = Math.atan2(uy, ux) * 180 / Math.PI;
          return <ellipse key={`${id}-${component}`} className="mask-radial-ellipse" cx={centerOutput.x} cy={centerOutput.y} rx={radiusX} ry={radiusY} transform={`rotate(${ellipseAngle} ${centerOutput.x} ${centerOutput.y})`} />;
        }
        const seg = record(shape.seg);
        const rect = box(seg.rect ?? shape.rect ?? shape.bounds);
        if (!rect) return null;
        const corners = [{ x: rect.x0, y: rect.y0 }, { x: rect.x1, y: rect.y0 }, { x: rect.x1, y: rect.y1 }, { x: rect.x0, y: rect.y1 }].map((value) => toSvg(project(value)));
        return <polygon key={`${id}-${component}`} className="mask-seg-outline" points={corners.map((value) => `${value.x},${value.y}`).join(" ")} />;
      })}
      {brushPreview && brushPreview.length ? <path className="mask-brush-path mask-brush-preview" d={`M ${brushPreview.map((value) => { const p = toSvg(project(value)); return `${p.x} ${p.y}`; }).join(" L ")}`} /> : null}
      {pins ? <g className="mask-pins">{visible.flatMap(({ shape, id, component }) => [
        ...shapePins(shape).map((value, pinIndex) => { const p = toSvg(project(value)); return <circle key={`${id}-${component}-pin-${pinIndex}`} className={`mask-pin ${String(shape.kind ?? "").toLowerCase() === "object" && pinIndex % 2 ? "mask-pin-exclude" : ""}`} cx={p.x} cy={p.y} r="2.5" />; }),
        ...(String(shape.kind ?? "") === "radial" ? radialHandles(shape, aspect).map((value, handleIndex) => { const p = toSvg(project(value)); return <circle key={`${id}-${component}-handle-${handleIndex}`} className="mask-radial-handle" cx={p.x} cy={p.y} r="3.5" />; }) : []),
      ])}</g> : null}
    </svg>
  </div>;
}

function SpotsOverlay({ develop, eyes }: { develop: DevelopShape; eyes?: boolean }) {
  const values = Array.isArray(eyes ? develop.red_eye ?? develop.redEye : develop.spots) ? (eyes ? develop.red_eye ?? develop.redEye : develop.spots) as unknown[] : [];
  return <div className="spots-overlay" aria-hidden="true">{values.map((value, index) => {
    const s = record(value);
    const center = point(s.center ?? s.position) ?? { x: 0.5, y: 0.5 };
    const rx = number(s.rx ?? s.radius, 0.045);
    const ry = number(s.ry ?? s.radius, rx);
    return <span key={`${eyes ? "eye" : "spot"}-${index}`} className={eyes ? "red-eye-shape" : "spot-shape"} style={{ left: `${center.x * 100}%`, top: `${center.y * 100}%`, width: `${rx * 200}%`, height: `${ry * 200}%` }} />;
  })}</div>;
}

function Navigator({ zoom, pan, onChange }: { zoom: number; pan: Point; onChange: (p: Point) => void }) {
  return <aside className="navigator" aria-label="Navigator">
    <div className="navigator-preview"><span style={{ left: `${pan.x * 100}%`, top: `${pan.y * 100}%`, width: `${Math.min(100, 100 / Math.max(1, zoom))}%`, height: `${Math.min(100, 100 / Math.max(1, zoom))}%` }} /></div>
    <div className="navigator-controls"><IconButton label="Pan left" onClick={() => onChange({ x: Math.max(0, pan.x - 0.05), y: pan.y })}>←</IconButton><IconButton label="Pan up" onClick={() => onChange({ x: pan.x, y: Math.max(0, pan.y - 0.05) })}>↑</IconButton><IconButton label="Pan down" onClick={() => onChange({ x: pan.x, y: Math.min(1, pan.y + 0.05) })}>↓</IconButton><IconButton label="Pan right" onClick={() => onChange({ x: Math.min(1, pan.x + 0.05), y: pan.y })}>→</IconButton></div>
  </aside>;
}

function MaskToolbar({ ui, setUi, run }: { ui: UiState; setUi: (patch: Partial<UiState>) => void; run: (id: string, params?: Record<string, unknown>) => Promise<unknown> }) {
  const addMask = (kind: string) => {
    setUi({ tool: kind, maskOverlay: true });
    void run("mask.add", { kind });
  };
  return <div className="mask-toolbar" role="toolbar" aria-label="Mask tools">{maskTools.map(([id, label]) => <Button variant="ghost" size="sm" key={id} type="button" className={ui.tool === id ? "selected" : ""} onClick={() => addMask(id)} title={label}>{label}</Button>)}<Button variant="ghost" size="sm" type="button" className={ui.maskOverlay ? "selected" : ""} onClick={() => setUi({ maskOverlay: !ui.maskOverlay })}>Overlay</Button><Button variant="ghost" size="sm" type="button" className={ui.maskPins ? "selected" : ""} onClick={() => setUi({ maskPins: !ui.maskPins })}>Pins</Button></div>;
}

function CropToolbar({ ui, setUi, run }: { ui: UiState; setUi: (patch: Partial<UiState>) => void; run: (id: string, params?: Record<string, unknown>) => Promise<unknown> }) {
  return <div className="mask-toolbar crop-toolbar" role="toolbar" aria-label="Crop options"><Button variant="ghost" size="sm" type="button" onClick={() => void run("crop.aspect", { aspect: "toggle" })}>Aspect</Button><Button variant="ghost" size="sm" type="button" onClick={() => void run("crop.rotateAspect", {})}>Rotate Aspect</Button><Button variant="ghost" size="sm" type="button" onClick={() => void run("crop.autoStraighten", {})}>Auto Straighten</Button><Button variant="ghost" size="sm" type="button" onClick={() => setUi({ cropOverlay: ui.cropOverlay === "off" ? "thirds" : "off" })} aria-pressed={ui.cropOverlay !== "off"}>Guides</Button><Button variant="ghost" size="sm" type="button" onClick={() => setUi({ cropOverlay: ui.cropOverlay === "thirds" ? "diagonal" : "thirds" })}>Guide Type</Button><Button variant="ghost" size="sm" type="button" onClick={() => void run("crop.reset", {})}>Reset Crop</Button></div>;
}

function RemoveToolbar({ ui, setUi, run }: { ui: UiState; setUi: (patch: Partial<UiState>) => void; run: (id: string, params?: Record<string, unknown>) => Promise<unknown> }) {
  return <div className="mask-toolbar crop-toolbar" role="toolbar" aria-label="Remove options"><Button variant="ghost" size="sm" type="button" className={ui.tool === "remove" ? "selected" : ""} onClick={() => setUi({ tool: "remove" })}>Remove</Button><Button variant="ghost" size="sm" type="button" className={ui.tool === "heal" ? "selected" : ""} onClick={() => setUi({ tool: "heal" })}>Heal</Button><Button variant="ghost" size="sm" type="button" className={ui.tool === "clone" ? "selected" : ""} onClick={() => setUi({ tool: "clone" })}>Clone</Button><Button variant="ghost" size="sm" type="button" onClick={() => void run("spot.findDust", { add: true })}>Find Dust</Button><Button variant="ghost" size="sm" type="button" onClick={() => void run("spot.delete", {})}>Delete Spot</Button></div>;
}

function ZoomMenu({ ui, setUi }: { ui: UiState; setUi: (patch: Partial<UiState>) => void }) {
  const zoomValue = typeof ui.zoom === "number" ? ui.zoom : ui.zoom === "fill" ? 1.25 : 1;
  const zoomLabel = ui.zoom === "fit" ? "Fit" : ui.zoom === "fill" ? "Fill" : `${Math.round(zoomValue * 100)}%`;
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  return <div className="zoom-menu"><Button variant="ghost" size="sm" ref={triggerRef} type="button" className="zoom-menu-trigger" aria-expanded={open} aria-haspopup="dialog" aria-label={`Zoom ${zoomLabel}`} onClick={() => setOpen((current) => !current)}>Zoom <strong>{zoomLabel}</strong><span aria-hidden="true">⌄</span></Button>{open && triggerRef.current ? <Popover anchor={triggerRef.current} label="Zoom controls" placement="top-start" width={218} className="zoom-menu-popover" onClose={() => setOpen(false)}><div className="zoom-menu-presets"><Button variant="ghost" size="sm" type="button" onClick={() => { setUi({ zoom: "fit" }); setOpen(false); }} aria-pressed={ui.zoom === "fit"}>Fit</Button><Button variant="ghost" size="sm" type="button" onClick={() => { setUi({ zoom: "fill" }); setOpen(false); }} aria-pressed={ui.zoom === "fill"}>Fill</Button><Button variant="ghost" size="sm" type="button" onClick={() => { setUi({ zoom: 1 }); setOpen(false); }} aria-pressed={ui.zoom === 1}>100%</Button></div><div className="zoom-slider"><span>Zoom</span><Slider label="Zoom" min={0.25} max={4} step={0.05} value={zoomValue} onChange={(zoom) => setUi({ zoom })} /><output>{Math.round(zoomValue * 100)}%</output></div><Button variant="ghost" size="sm" className="zoom-navigator" type="button" onClick={() => { setUi({ navigator: !ui.navigator }); setOpen(false); }} aria-pressed={ui.navigator}>Navigator</Button></Popover> : null}</div>;
}

/** One of the Before / After layouts the footer button stands for; Show Original (`\`) is a view of its own. */
function isBeforeAfterLayout(mode: UiState["beforeAfter"]): boolean {
  return mode !== "off" && mode !== "original";
}

function Footer({ ui, setUi, run, onNative }: { ui: UiState; setUi: (patch: Partial<UiState>) => void; run: (id: string, params?: Record<string, unknown>) => Promise<unknown>; onNative: (action: string, params?: Record<string, unknown>) => Promise<unknown> }) {
  const nextInfoOverlay = () => {
    const mode = Number.isFinite(ui.infoOverlay) ? Math.trunc(ui.infoOverlay) : 0;
    setUi({ infoOverlay: (Math.max(0, Math.min(2, mode)) + 1) % 3 });
  };
  return <footer className="stage-footer"><div className="footer-left"><Button variant="ghost" size="sm" type="button" onClick={() => setUi({ filmstrip: !ui.filmstrip })} aria-pressed={ui.filmstrip}>Filmstrip</Button><Button variant="ghost" size="sm" type="button" onClick={() => setUi({ beforeAfter: isBeforeAfterLayout(ui.beforeAfter) ? "off" : "sideBySide" })} aria-pressed={isBeforeAfterLayout(ui.beforeAfter)}>Before / After</Button><Button variant="ghost" size="sm" type="button" onClick={() => setUi({ clipping: !ui.clipping })} aria-pressed={ui.clipping}>Clipping</Button><Button variant="ghost" size="sm" type="button" onClick={() => setUi({ softProof: !ui.softProof })} aria-pressed={ui.softProof}>Proof</Button></div><div className="footer-center"><ZoomMenu ui={ui} setUi={setUi} /></div><div className="footer-right"><Button variant="ghost" size="sm" type="button" onClick={nextInfoOverlay} aria-pressed={ui.infoOverlay !== 0}>Info</Button><Button variant="ghost" size="sm" type="button" onClick={() => setUi({ slideshow: !ui.slideshow })}>{ui.slideshow ? "Pause" : "Slideshow"}</Button><Button variant="ghost" size="sm" type="button" onClick={() => void onNative("fullscreen", { enabled: true })}>Fullscreen</Button><Button variant="ghost" size="sm" type="button" onClick={() => void onNative("secondWindow", { view: "detail" })}>Second window</Button><Button variant="ghost" size="sm" type="button" onClick={() => void run("view.beforeAfter", {})}>Compare</Button></div></footer>;
}

function prettyDate(value: string): string {
  return value.replace("T", " ").slice(0, 16);
}

function InfoOverlay({ photo, mode, onClose }: { photo: PhotoSummary | null; mode: number; onClose: () => void }) {
  if (!photo) return null;
  const lines: string[] = [photo.fileName];
  if (mode === 1) {
    const date = photo.captured || photo.imported;
    lines.push(date ? `${prettyDate(date)} · ${photo.w} × ${photo.h}` : `${photo.w} × ${photo.h}`);
  } else if (mode === 2) {
    const exposure: string[] = [];
    if (photo.shutter) exposure.push(`${photo.shutter} s`);
    if (typeof photo.aperture === "number" && Number.isFinite(photo.aperture)) exposure.push(`f/${photo.aperture.toFixed(1)}`);
    if (typeof photo.iso === "number" && Number.isFinite(photo.iso)) exposure.push(`ISO ${photo.iso}`);
    if (typeof photo.focalMm === "number" && Number.isFinite(photo.focalMm)) exposure.push(`${photo.focalMm.toFixed(0)} mm`);
    lines.push(exposure.length ? exposure.join("  ") : "No exposure information");
    const camera = [photo.camera, photo.lens].filter((value) => value).join(" · ");
    if (camera) lines.push(camera);
  }
  return <aside className="info-overlay"><IconButton label="Close photo info" onClick={onClose}>×</IconButton>{lines.map((line, index) => index === 0 ? <strong key={line}>{line}</strong> : <span key={`${line}-${index}`}>{line}</span>)}</aside>;
}

function MultiViewActions({ view, candidate, reference, run, setUi }: { view: ViewMode; candidate: number | null; reference: number | null; run: (id: string, params?: Record<string, unknown>) => Promise<unknown>; setUi: (patch: Partial<UiState>) => void }) {
  if (view !== "compare" && view !== "reference") return null;
  return <div className="multi-view-actions"><Button variant="ghost" size="sm" type="button" onClick={() => void run("compare.swap", {})}>Swap</Button><Button variant="ghost" size="sm" type="button" disabled={candidate == null} onClick={() => void run("compare.makeSelect", {})}>Make Select</Button>{view === "reference" ? <Button variant="ghost" size="sm" type="button" onClick={() => { setUi({ referenceId: reference }); void run("photo.setReference", { id: reference }); }}>Keep Reference</Button> : null}</div>;
}

function BeforeAfterStage({ mode, photoId, photo, previewEdge, viewGeneration, develop, onHistogram, onPointerDown, onPointerMove, onPointerUp, onPointerCancel }: { mode: UiState["beforeAfter"]; photoId: number | null; photo: PhotoSummary | null; previewEdge: number; viewGeneration: number; develop: DevelopShape; onHistogram: (value: unknown) => void; onPointerDown: (event: PointerEvent<HTMLElement>) => void; onPointerMove: (event: PointerEvent<HTMLElement>) => void; onPointerUp: (event: PointerEvent<HTMLElement>) => void; onPointerCancel: (event: PointerEvent<HTMLElement>) => void }) {
  const vertical = mode === "topBottom" || mode === "splitTopBottom";
  return <div className={`before-after-panes ${vertical ? "vertical" : "horizontal"} ${mode === "split" || mode === "splitTopBottom" ? "split-mode" : ""}`}><div className="before-after-pane"><StageImage photoId={photoId} photo={photo} previewEdge={previewEdge} slot="before" before viewGeneration={viewGeneration} develop={develop} onHistogram={onHistogram} /><span className="pane-label">Before</span></div><div className="before-after-pane"><StageImage photoId={photoId} photo={photo} previewEdge={previewEdge} slot="after" viewGeneration={viewGeneration} develop={develop} onHistogram={onHistogram} onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={onPointerUp} onPointerCancel={onPointerCancel} /><span className="pane-label">After</span></div></div>;
}

export function StageWorkspace() {
  const { snapshot, ui, setUi, run, native, histogram, setHistogram } = useDesktop();
  const stageRef = useRef<HTMLDivElement>(null);
  const active = snapshot?.active ?? null;
  const develop = record(snapshot?.develop);
  const previewGeneration = snapshot?.viewGeneration ?? 0;
  const [pan, setPan] = useState<Point>({ x: 0, y: 0 });
  const [drag, setDrag] = useState<{ kind: string; start: Point; last: Point; points?: Point[]; startBox?: Box; handle?: number; radialHandle?: "rx" | "ry" | "rotate"; startAngle?: number; startPointerAngle?: number; maskId?: number; component?: number; startShape?: DevelopShape } | null>(null);
  const [candidateIndex, setCandidateIndex] = useState(0);
  const burst = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pending = useRef<{ id: string; params: Record<string, unknown> } | null>(null);
  const commandQueue = useRef(Promise.resolve());
  const interaction = useRef(false);

  const selection: number[] = useMemo(() => snapshot?.selection ?? [], [snapshot?.selection]);
  const candidateIds = selection.filter((id) => id !== active);
  const candidate = candidateIds[candidateIndex % Math.max(1, candidateIds.length)] ?? null;
  const detailIds = useMemo(() => {
    const visibleIds = ui.view === "survey" ? selection
      : ui.view === "compare" ? [candidate]
      : ui.view === "reference" ? [ui.referenceId] : [];
    return Array.from(new Set([active, ...visibleIds].filter((id): id is number => typeof id === "number")));
  }, [active, candidate, selection, ui.referenceId, ui.view]);
  const inspected = useInspectedPhotos(detailIds);
  const photoFor = useCallback((id: number | null) => photoForId(snapshot, id) ?? (id == null ? null : inspected.get(id) ?? null), [inspected, snapshot]);
  const photo = photoFor(active);
  const zoom = typeof ui.zoom === "number" ? ui.zoom : ui.zoom === "fill" ? 1.25 : 1;
  const sourceAspect = photo && photo.h > 0 ? photo.w / photo.h : 1.5;
  const previewEdge = Math.max(256, Math.min(8192, Math.round(Number.isFinite(ui.previewEdge) ? ui.previewEdge : 2560)));
  const activeMask = number((snapshot as (DesktopSnapshot & { activeMask?: unknown }) | null)?.activeMask, number(record(snapshot?.source).activeMask ?? record(snapshot?.source).active_mask, -1));

  const send = useCallback((id: string, params: Record<string, unknown> = {}) => {
    const next = commandQueue.current.then(() => run(id, params));
    commandQueue.current = next.then(() => undefined, () => undefined);
    return next;
  }, [run]);

  const safeSend = useCallback(async (id: string, params: Record<string, unknown> = {}) => {
    try { await send(id, params); } catch { /* cleanup must complete after command failures */ }
  }, [send]);

  const commitBurst = useCallback(async () => {
    if (burst.current) clearTimeout(burst.current);
    const next = pending.current;
    pending.current = null;
    if (next) await send(next.id, next.params);
  }, [send]);

  const gestureValue = useCallback((id: string, params: Record<string, unknown>) => {
    pending.current = { id, params };
    if (burst.current) clearTimeout(burst.current);
    burst.current = setTimeout(commitBurst, 400);
  }, [commitBurst]);

  useEffect(() => () => {
    if (burst.current) clearTimeout(burst.current);
    burst.current = null;
    pending.current = null;
    if (interaction.current) void safeSend("develop.cancelInteraction", {});
  }, [safeSend]);

  const pointAt = useCallback((event: PointerEvent<HTMLElement>) => invertPhotoPoint(event, event.currentTarget, develop, sourceAspect, zoom, pan), [develop, pan, sourceAspect, zoom]);

  const beginPointer = useCallback((event: PointerEvent<HTMLElement>) => {
    if (active == null) return;
    interaction.current = true;
    const p = pointAt(event);
    const crop = cropOf(develop).rect;
    if (ui.panel === "crop") {
      event.currentTarget.setPointerCapture(event.pointerId);
      const angle = cropOf(develop).angle;
      const straight = rotatePoint(p, angle, orientedAspectFor(develop, sourceAspect));
      const handle = cropHandleAt(straight, crop);
      const center = { x: (crop.x0 + crop.x1) / 2, y: (crop.y0 + crop.y1) / 2 };
      const pointerAngle = Math.atan2(straight.y - center.y, straight.x - center.x);
      const kind = handle == null && (straight.x < crop.x0 || straight.x > crop.x1 || straight.y < crop.y0 || straight.y > crop.y1) ? "cropAngle" : "crop";
      setDrag({ kind, start: straight, last: straight, startBox: crop, handle: handle ?? 8, startAngle: angle, startPointerAngle: pointerAngle });
      void send("develop.beginInteraction", { label: "Crop" });
      return;
    }
    if (ui.panel === "remove") { event.currentTarget.setPointerCapture(event.pointerId); setDrag({ kind: "spot", start: p, last: p }); void send("develop.beginInteraction", { label: "Remove" }); return; }
    if (ui.panel === "redeye") { event.currentTarget.setPointerCapture(event.pointerId); setDrag({ kind: "eye", start: p, last: p }); void send("develop.beginInteraction", { label: "Red Eye" }); return; }
    if (ui.panel === "masking") {
      event.currentTarget.setPointerCapture(event.pointerId);
      const tool = ui.tool === "selection" ? "object" : ui.tool || "brush";
      const radial = tool === "radial" ? maskShapes(develop, "selected", activeMask).find(({ shape }) => String(shape.kind ?? "") === "radial") : undefined;
      const radialAction = radial ? radialHandleAt(p, radial.shape, orientedAspectFor(develop, sourceAspect)) : null;
      const kind = radialAction === "move" ? "radialMove" : radialAction === "rx" || radialAction === "ry" || radialAction === "rotate" ? "radialResize" : tool;
      setDrag({ kind, start: p, last: p, points: tool === "brush" ? [p] : undefined, radialHandle: radialAction === "rx" || radialAction === "ry" || radialAction === "rotate" ? radialAction : undefined, maskId: radial?.id, component: radial?.component, startShape: radial?.shape });
      void send("develop.beginInteraction", { label: "Mask" });
      if (tool === "colorRange") void send("mask.sampleColor", { x: p.x, y: p.y, add: event.shiftKey, ...(activeMask > 0 ? { id: activeMask } : {}) });
      if (tool === "object") void send("mask.objectPoint", { x: p.x, y: p.y, exclude: event.altKey, ...(activeMask > 0 ? { id: activeMask } : {}) });
    }
    if (ui.panel === "edit" && ui.tool === "wbPicker") { void send("develop.wbPick", { x: p.x, y: p.y }); setUi({ tool: "" }); }
  }, [active, activeMask, develop, pointAt, send, setUi, sourceAspect, ui.panel, ui.tool]);

  const movePointer = useCallback((event: PointerEvent<HTMLElement>) => {
    if (!drag) return;
    const p = pointAt(event);
    if (drag.kind === "crop" && drag.startBox) {
      const straight = rotatePoint(p, cropOf(develop).angle, orientedAspectFor(develop, sourceAspect));
      const dx = straight.x - drag.start.x;
      const dy = straight.y - drag.start.y;
      const next = { x0: drag.startBox.x0, y0: drag.startBox.y0, x1: drag.startBox.x1, y1: drag.startBox.y1 };
      if (drag.handle === 0 || drag.handle === 7 || drag.handle === 3) next.x0 += dx;
      if (drag.handle === 1 || drag.handle === 5 || drag.handle === 2) next.x1 += dx;
      if (drag.handle === 0 || drag.handle === 4 || drag.handle === 1) next.y0 += dy;
      if (drag.handle === 3 || drag.handle === 6 || drag.handle === 2) next.y1 += dy;
      if (drag.handle === 8) { next.x0 += dx; next.x1 += dx; next.y0 += dy; next.y1 += dy; }
      const x0 = Math.max(0, Math.min(0.98, Math.min(next.x0, next.x1 - 0.02)));
      const y0 = Math.max(0, Math.min(0.98, Math.min(next.y0, next.y1 - 0.02)));
      const x1 = Math.min(1, Math.max(0.02, Math.max(next.x1, next.x0 + 0.02)));
      const y1 = Math.min(1, Math.max(0.02, Math.max(next.y1, next.y0 + 0.02)));
      gestureValue("crop.set", { rect: [x0, y0, x1, y1] });
    } else if (drag.kind === "cropAngle" && drag.startBox != null && drag.startAngle != null && drag.startPointerAngle != null) {
      const straight = rotatePoint(p, cropOf(develop).angle, orientedAspectFor(develop, sourceAspect));
      const center = { x: (drag.startBox.x0 + drag.startBox.x1) / 2, y: (drag.startBox.y0 + drag.startBox.y1) / 2 };
      const currentAngle = Math.atan2(straight.y - center.y, straight.x - center.x);
      const degrees = drag.startAngle + ((currentAngle - drag.startPointerAngle) * 180) / Math.PI;
      gestureValue("crop.straighten", { angle: Math.max(-45, Math.min(45, degrees)) });
    } else if (drag.kind === "brush") {
      const points = [...(drag.points ?? []), p];
      setDrag((current) => current ? { ...current, last: p, points } : current);
    } else if (drag.kind === "linear") {
      gestureValue("mask.update", { ...(drag.maskId != null ? { id: drag.maskId } : {}), ...(drag.component != null ? { component: drag.component } : {}), shape: { kind: "linear", start: { x: drag.start.x, y: drag.start.y }, end: { x: p.x, y: p.y } } });
    } else if (drag.kind === "radial") {
      gestureValue("mask.update", { ...(drag.maskId != null ? { id: drag.maskId } : {}), ...(drag.component != null ? { component: drag.component } : {}), shape: radialShapeFromDrag(drag.start, p, sourceAspect, String(develop.orientation ?? "normal")) });
    } else if ((drag.kind === "radialMove" || drag.kind === "radialResize") && drag.startShape) {
      const shape = { ...drag.startShape };
      const center = shapePoint(shape.center) ?? drag.start;
      const startAngle = number(shape.angle);
      if (drag.kind === "radialMove") {
        const dx = p.x - drag.start.x;
        const dy = p.y - drag.start.y;
        shape.center = { x: Math.max(0, Math.min(1, center.x + dx)), y: Math.max(0, Math.min(1, center.y + dy)) };
      } else if (drag.radialHandle === "rotate") {
        const aspect = orientedAspectFor(develop, sourceAspect);
        const beforeLong = normalizedToLong({ x: drag.start.x - center.x, y: drag.start.y - center.y }, aspect);
        const afterLong = normalizedToLong({ x: p.x - center.x, y: p.y - center.y }, aspect);
        const before = Math.atan2(beforeLong.y, beforeLong.x);
        const after = Math.atan2(afterLong.y, afterLong.x);
        shape.angle = startAngle + (after - before) * 180 / Math.PI;
      } else {
        const angle = -startAngle * Math.PI / 180;
        const localLong = normalizedToLong({ x: p.x - center.x, y: p.y - center.y }, orientedAspectFor(develop, sourceAspect));
        const local = { x: localLong.x * Math.cos(angle) - localLong.y * Math.sin(angle), y: localLong.x * Math.sin(angle) + localLong.y * Math.cos(angle) };
        if (drag.radialHandle === "rx") shape.rx = Math.max(0.005, Math.abs(local.x));
        if (drag.radialHandle === "ry") shape.ry = Math.max(0.005, Math.abs(local.y));
      }
      gestureValue("mask.update", { ...(drag.maskId != null ? { id: drag.maskId } : {}), ...(drag.component != null ? { component: drag.component } : {}), shape });
    } else if (drag.kind === "spot") {
      setDrag((current) => current ? { ...current, last: p } : current);
    } else if (drag.kind === "eye") {
      setDrag((current) => current ? { ...current, last: p } : current);
    }
  }, [develop, drag, gestureValue, pointAt, sourceAspect]);

  const endPointer = useCallback(async (event: PointerEvent<HTMLElement>) => {
    if (!drag) return;
    const p = pointAt(event);
    try {
      if (drag.kind === "brush" && drag.points?.length) {
        await send("mask.brushStroke", { ...(drag.maskId != null ? { id: drag.maskId } : {}), points: drag.points.map((value) => [value.x, value.y]), size: ui.brushSize / 1000, feather: ui.brushFeather / 100 });
      } else if (drag.kind === "spot") {
        const radius = Math.max(0.005, Math.abs(p.x - drag.start.x) || 0.025);
        const mode = ui.tool === "clone" ? "clone" : ui.tool === "heal" ? "heal" : "remove";
        await send("spot.add", { mode, points: [[drag.start.x, drag.start.y]], size: radius, source: [p.x - drag.start.x, p.y - drag.start.y] });
      } else if (drag.kind === "eye") {
        await send("redeye.add", { center: [drag.start.x, drag.start.y], rx: Math.max(0.01, Math.abs(p.x - drag.start.x)), ry: Math.max(0.01, Math.abs(p.y - drag.start.y)) });
      }
      await commitBurst();
    } catch {
      // Queue continues; endInteraction still closes native gesture below.
    } finally {
      await safeSend("develop.endInteraction", {});
      interaction.current = false;
      setDrag(null);
    }
  }, [commitBurst, drag, pointAt, safeSend, send, ui.brushFeather, ui.brushSize, ui.tool]);

  const cancelPointer = useCallback((event?: PointerEvent<HTMLElement>) => {
    if (event && event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
    if (burst.current) clearTimeout(burst.current);
    burst.current = null;
    pending.current = null;
    interaction.current = false;
    void safeSend("develop.cancelInteraction", {});
    setDrag(null);
  }, [safeSend]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && drag) {
        if (burst.current) clearTimeout(burst.current);
        burst.current = null;
        pending.current = null;
        cancelPointer();
        return;
      }
      if (event.defaultPrevented || event.isComposing || isEditableTarget(event.target)) return;
      if (event.key === "Escape") { setUi({ infoOverlay: 0, slideshow: false, tool: ui.tool === "wbPicker" ? "" : ui.tool }); return; }
      if (event.key === " ") { event.preventDefault(); setUi({ slideshow: !ui.slideshow }); return; }
      if (event.key === "ArrowLeft" || event.key === "ArrowRight") { event.preventDefault(); void send(event.key === "ArrowLeft" ? "library.previous" : "library.next", {}); }
      if (event.key.toLowerCase() === "w" && ui.panel === "edit") setUi({ tool: "wbPicker" });
      if (event.key.toLowerCase() === "o" && ui.panel === "crop") setUi({ cropOverlay: ui.cropOverlay === "thirds" ? "diagonal" : "thirds" });
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [cancelPointer, drag, send, setUi, ui.cropOverlay, ui.panel, ui.slideshow, ui.tool]);

  const routeView = useCallback((view: ViewMode) => {
    setUi({ view, panel: view === "people" ? "info" : ui.panel });
    void (async () => {
      await send(`view.${view}`, {});
      if (view === "reference" && active != null && candidate != null && candidate !== active) {
        setUi({ referenceId: active });
        await send("library.select", { ids: [candidate], active: candidate, mode: "replace" });
      }
    })();
  }, [active, candidate, send, setUi, ui.panel]);
  const themeClass = ui.theme === "dark" ? "stage-dark" : ui.theme === "light" ? "stage-light" : "stage-system";
  const stageClass = `stage-workspace stage-${ui.view} ${themeClass} ${ui.beforeAfter !== "off" ? "has-before-after" : ""}`;

  return <section ref={stageRef} className={stageClass} aria-label="Photo stage">
    <div className="stage-topbar"><label className="stage-view-mode"><span>View</span><Select label="View mode" value={views.some((view) => view.id === ui.view) ? ui.view : "detail"} options={views.map((view) => ({ value: view.id, label: view.label }))} onChange={(value) => routeView(value as ViewMode)} /></label><div className="stage-photo-title"><strong>{titleFor(photo)}</strong><span>{photo ? `${photo.w} × ${photo.h}` : ""}</span></div><MultiViewActions view={ui.view} candidate={candidate} reference={ui.referenceId} run={run} setUi={setUi} /><div className="stage-status" aria-live="polite">{snapshot?.status.previewBuild ? <span className="stage-loading">Rendering preview…</span> : snapshot?.status.unsaved ? <span className="stage-unsaved">Unsaved changes</span> : snapshot ? <span className="stage-saved">Saved</span> : null}{snapshot?.status.importing ? <span>Importing…</span> : null}{snapshot?.status.exporting ? <span>Exporting…</span> : null}</div></div>
    <div className="stage-body">
      <div className="stage-canvas" style={{ ["--stage-zoom" as string]: zoom, ["--stage-pan-x" as string]: `${pan.x * 100}%`, ["--stage-pan-y" as string]: `${pan.y * 100}%` }}>
        {ui.view === "compare" ? <div className="compare-panes"><div className="compare-pane"><StageImage photoId={active} photo={photoFor(active)} previewEdge={previewEdge} slot={previewSlot(ui.view)} before={ui.beforeAfter === "original"} viewGeneration={previewGeneration} develop={develop} onHistogram={setHistogram} onPointerDown={beginPointer} onPointerMove={movePointer} onPointerUp={endPointer} onPointerCancel={cancelPointer} /><span className="pane-label">Select</span></div><div className="compare-pane"><StageImage photoId={candidate} photo={photoFor(candidate)} previewEdge={previewEdge} slot={previewSlot(ui.view, 1)} viewGeneration={previewGeneration} develop={develop} onHistogram={setHistogram} /><span className="pane-label">Candidate {candidate ?? "—"}</span></div></div> : ui.view === "reference" ? <div className="compare-panes"><div className="compare-pane"><StageImage photoId={ui.referenceId} photo={photoFor(ui.referenceId)} previewEdge={previewEdge} slot="reference" viewGeneration={previewGeneration} develop={develop} onHistogram={setHistogram} /><span className="pane-label">Reference {ui.referenceId ?? "—"}</span></div><div className="compare-pane"><StageImage photoId={active} photo={photoFor(active)} previewEdge={previewEdge} slot="reference-active" viewGeneration={previewGeneration} develop={develop} onHistogram={setHistogram} onPointerDown={beginPointer} onPointerMove={movePointer} onPointerUp={endPointer} onPointerCancel={cancelPointer} /><span className="pane-label">Active</span></div></div> : ui.view === "survey" ? <div className="survey-grid">{(selection.length ? selection : active == null ? [] : [active]).map((id, index) => <Card flush key={id} className={id === active ? "survey-photo selected" : "survey-photo"} selected={id === active} label={`Photo ${index + 1}`} onClick={() => { setUi({ view: "detail" }); void send("library.select", { ids: [id], active: id, mode: "replace" }); }}><StageImage photoId={id} photo={photoFor(id)} previewEdge={previewEdge} slot={previewSlot(ui.view, index)} viewGeneration={previewGeneration} develop={develop} onHistogram={setHistogram} /><span>{index + 1}</span></Card>)}</div> : ui.view === "people" ? <div className="people-stage"><StageImage photoId={active} photo={photoFor(active)} previewEdge={previewEdge} slot={previewSlot(ui.view)} viewGeneration={previewGeneration} develop={develop} onHistogram={setHistogram} /><div className="face-boxes">{Array.isArray(record(snapshot?.source).faces) ? (record(snapshot?.source).faces as unknown[]).map((face, index) => { const f = record(face); const r = box(f.rect ?? f.bounds) ?? { x0: 0.3 + index * 0.05, y0: 0.25, x1: 0.44 + index * 0.05, y1: 0.42 }; return <span key={index} style={{ left: `${r.x0 * 100}%`, top: `${r.y0 * 100}%`, width: `${(r.x1 - r.x0) * 100}%`, height: `${(r.y1 - r.y0) * 100}%` }} />; }) : null}</div></div> : ui.beforeAfter !== "off" && ui.beforeAfter !== "original" ? <BeforeAfterStage mode={ui.beforeAfter} photoId={active} photo={photoFor(active)} previewEdge={previewEdge} viewGeneration={previewGeneration} develop={develop} onHistogram={setHistogram} onPointerDown={beginPointer} onPointerMove={movePointer} onPointerUp={endPointer} onPointerCancel={cancelPointer} /> : <div className="single-pane"><StageImage photoId={active} photo={photoFor(active)} previewEdge={previewEdge} slot={previewSlot(ui.view)} before={ui.beforeAfter === "original"} viewGeneration={previewGeneration} develop={develop} onHistogram={setHistogram} onPointerDown={beginPointer} onPointerMove={movePointer} onPointerUp={endPointer} onPointerCancel={cancelPointer} />{ui.panel === "crop" ? <CropOverlay develop={develop} guide={ui.cropOverlay} /> : null}{ui.maskOverlay && ui.panel === "masking" ? <MaskOverlay develop={develop} activeMask={activeMask} mode={ui.maskOverlayMode} pins={ui.maskPins} sourceAspect={sourceAspect} zoom={zoom} pan={pan} brushPreview={drag?.kind === "brush" ? drag.points : undefined} /> : null}{ui.panel === "remove" ? <SpotsOverlay develop={develop} /> : null}{ui.panel === "redeye" ? <SpotsOverlay develop={develop} eyes /> : null}{ui.clipping ? <div className="clipping-overlay" aria-label="Clipping preview" /> : null}</div>}
        {ui.infoOverlay ? <InfoOverlay photo={photo} mode={ui.infoOverlay} onClose={() => setUi({ infoOverlay: 0 })} /> : null}
        {ui.navigator ? <Navigator zoom={zoom} pan={pan} onChange={setPan} /> : null}
        {histogram ? <div className="histogram-badge" aria-label="Histogram available">Histogram</div> : null}
      </div>
      {ui.panel === "crop" ? <CropToolbar ui={ui} setUi={setUi} run={run} /> : null}
      {ui.panel === "remove" ? <RemoveToolbar ui={ui} setUi={setUi} run={run} /> : null}
      {ui.panel === "masking" ? <MaskToolbar ui={ui} setUi={setUi} run={run} /> : null}
    </div>
    {ui.filmstrip ? <div className="stage-filmstrip"><Filmstrip /></div> : null}
    <Footer ui={ui} setUi={setUi} run={run} onNative={native} />
  </section>;
}

export default StageWorkspace;
