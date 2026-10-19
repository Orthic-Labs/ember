import { useEffect, type RefObject } from 'react';

/**
 * Exception B4 (M2 ownership): while a dialog is open, every other child of `document.body` is inert and
 * aria-hidden. ModalShell traps Tab and restores focus, but it does not mark the page behind the dialog inert,
 * so pointer and assistive-technology focus could still reach it.
 * `anchor` is any element inside the open dialog; the body child that contains it stays live.
 */
export function useInertSiblings(anchor: RefObject<HTMLElement | null>): void {
  useEffect(() => {
    const own = anchor.current;
    if (!own) return;
    const portal = Array.from(document.body.children).find((node) => node.contains(own));
    const siblings = Array.from(document.body.children).filter((node) => node !== portal);
    const previous = siblings.map((node) => ({ node, inert: (node as HTMLElement).inert, hidden: node.getAttribute('aria-hidden') }));
    siblings.forEach((node) => { (node as HTMLElement).inert = true; node.setAttribute('aria-hidden', 'true'); });
    return () => {
      previous.forEach(({ node, inert, hidden }) => {
        (node as HTMLElement).inert = inert;
        if (hidden === null) node.removeAttribute('aria-hidden');
        else node.setAttribute('aria-hidden', hidden);
      });
    };
  }, [anchor]);
}
