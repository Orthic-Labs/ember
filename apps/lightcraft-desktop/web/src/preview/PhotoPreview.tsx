import type { CSSProperties, ImgHTMLAttributes } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc } from "../api";
import type { PreviewQuality } from "../desktop/types";
import { usePreview } from "./usePreview";
import { Button } from "@rightkit/react-shell/react";

export interface PhotoPreviewProps extends Omit<ImgHTMLAttributes<HTMLImageElement>, "src" | "width" | "height" | "onError"> {
  photoId: number;
  slot: string;
  width: number;
  height: number;
  viewGeneration?: number;
  quality?: PreviewQuality;
  before?: boolean;
  onHistogram?: (histogram: unknown) => void;
}

interface PresentedImage {
  descriptor: NonNullable<ReturnType<typeof usePreview>["state"]["descriptor"]>;
  url: string;
}

/** Real native preview image. Loading/error labels remain available to keyboard and screen readers. */
export function PhotoPreview({ photoId, slot, width, height, viewGeneration = 0, quality = "full", before = false, className, style, onHistogram, alt = "", ...imageProps }: PhotoPreviewProps) {
  const preview = usePreview({ photoId, slot, width, height, viewGeneration, quality, before, onHistogram });
  const url = useMemo(() => {
    if (!preview.state.url || preview.state.descriptor?.photoId !== photoId) return null;
    return convertFileSrc(preview.state.url.replace("lightcraft-preview://", ""), "lightcraft-preview");
  }, [photoId, preview.state.descriptor?.photoId, preview.state.url]);
  const frameStyle: CSSProperties = { width: "100%", height: "100%", minWidth: 1, minHeight: 1, ...style };
  const descriptor = preview.state.descriptor;
  const candidate = descriptor && url ? { descriptor, url } : null;
  const [presented, setPresented] = useState<PresentedImage | null>(null);
  const presentedRef = useRef<PresentedImage | null>(null);
  const lastPhoto = useRef(photoId);

  useEffect(() => {
    if (lastPhoto.current === photoId) return;
    lastPhoto.current = photoId;
    presentedRef.current = null;
    setPresented(null);
  }, [photoId]);

  const promote = useCallback((image: PresentedImage) => {
    if (image.descriptor.photoId !== photoId) return;
    if (!preview.onImageReady(image.descriptor)) return;
    presentedRef.current = image;
    setPresented(image);
  }, [photoId, preview.onImageReady]);

  const renderImage = (image: PresentedImage, visible: boolean) => {
    const isCandidate = candidate?.descriptor.handle === image.descriptor.handle;
    // Both layers may paint: the already-decoded candidate covers retained pixels
    // when its mounted image is ready. Do not hide it with opacity while retiring
    // the old handle; native webviews can paint that handoff before promotion commits.
    const layeredStyle: CSSProperties = { ...frameStyle, position: "absolute", inset: 0, zIndex: visible ? 1 : 2 };
    const onLoad: ImgHTMLAttributes<HTMLImageElement>["onLoad"] = (event) => {
      imageProps.onLoad?.(event);
      if (isCandidate && event.currentTarget.getAttribute("src") === image.url) promote(image);
    };
    const onError: ImgHTMLAttributes<HTMLImageElement>["onError"] = isCandidate
      ? (event) => {
        if (event.currentTarget.getAttribute("src") !== image.url) return;
        if (presentedRef.current?.descriptor.handle === image.descriptor.handle) {
          presentedRef.current = null;
          setPresented(null);
        }
        preview.onImageError(image.descriptor);
      }
      : undefined;
    return <img key={image.descriptor.handle} {...imageProps} {...(alt === "" ? { alt: "Photo preview" } : { alt })} className={className} style={visible && !presented?.descriptor.handle ? frameStyle : layeredStyle} src={image.url} width={width} height={height} decoding="async" draggable={false} onLoad={onLoad} onError={onError} />;
  };

  const currentPresented = presented?.descriptor.photoId === photoId ? presented : null;
  const handoff = candidate && currentPresented && candidate.descriptor.handle !== currentPresented.descriptor.handle;
  const wrapperStyle: CSSProperties = { ...frameStyle, position: "relative", overflow: "hidden", isolation: "isolate" };
  const images = candidate
    ? handoff
      ? [renderImage(currentPresented!, true), renderImage(candidate, false)]
      : [renderImage(candidate, true)]
    : currentPresented
      ? [renderImage(currentPresented, true)]
      : [];
  const image = images.length > 0;
  const error = preview.state.status === "error";
  return (
    <div className="photo-preview-frame" style={wrapperStyle} role={error && image ? "group" : !image ? (error ? "img" : "status") : undefined} aria-label={error ? `${alt || "Photo"}: ${preview.state.error}` : !image ? "Loading preview" : undefined}>
      {images}
      {!image && <span data-preview-state={error ? "error" : "loading"}>{error ? "Preview unavailable" : "Loading preview…"}</span>}
      {error && image && <div role="alert" style={{ position: "absolute", zIndex: 3, inset: "auto 8px 8px", display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8, padding: "6px 8px", background: "rgba(0, 0, 0, 0.72)", color: "white" }}><span data-preview-state="error">{preview.state.error}</span><Button size="sm" onClick={preview.retry}>Retry</Button></div>}
      {error && !image && <Button size="sm" onClick={preview.retry}>Retry</Button>}
    </div>
  );
}

export default PhotoPreview;
