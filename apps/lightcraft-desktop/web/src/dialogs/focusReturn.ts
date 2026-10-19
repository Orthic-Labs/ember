import { useEffect, useRef } from 'react';

/**
 * Focus that returns to where the user was before a dialog, even when WebKit never focused the button that opened
 * it (macOS) or the opener was a command palette that unmounts as the dialog mounts. ModalShell restores focus to
 * whatever was focused when it rendered; that is body in both cases, so `useReturnFocus` remembers the last real
 * control the user used outside any overlay and puts focus back there when ModalShell left it on body.
 */
let lastNonOverlayFocus: HTMLElement | null = null;
const CONTROL = 'button,a[href],input,select,textarea,[tabindex]:not([tabindex="-1"])';

function restorable(value: Element | null): value is HTMLElement {
  if (!(value instanceof HTMLElement) || !value.isConnected || value === document.body) return false;
  if (value.closest('.rk-overlay, .rk-popover, .lc-dialog-backdrop, [aria-hidden="true"]')) return false;
  for (let node: HTMLElement | null = value; node; node = node.parentElement) if (node.inert) return false;
  return true;
}

function control(target: EventTarget | null): Element | null {
  if (!(target instanceof Element)) return null;
  return target.matches(CONTROL) ? target : target.closest(CONTROL);
}

/** Track the last restorable control that took focus or a press. Mount once, above every dialog. */
export function useFocusHistory(): void {
  useEffect(() => {
    const remember = (event: Event) => {
      const target = control(event.target);
      if (restorable(target)) lastNonOverlayFocus = target;
    };
    document.addEventListener('focusin', remember, true);
    document.addEventListener('pointerdown', remember, true);
    return () => {
      document.removeEventListener('focusin', remember, true);
      document.removeEventListener('pointerdown', remember, true);
    };
  }, []);
}

/** Call in a dialog frame: on unmount, when focus was lost to body, return it to the pre-dialog control. */
export function useReturnFocus(): void {
  const target = useRef<HTMLElement | null>(null);
  if (target.current === null) {
    const active = document.activeElement;
    target.current = restorable(active) ? active : lastNonOverlayFocus;
  }
  useEffect(() => {
    const remembered = target;
    return () => {
      // After ModalShell's own microtask restore and the inert siblings' cleanup, so a valid restore is never undone.
      setTimeout(() => {
        const next = remembered.current;
        const active = document.activeElement;
        if ((!active || active === document.body) && next?.isConnected && restorable(next)) next.focus({ preventScroll: true });
      }, 0);
    };
  }, []);
}
