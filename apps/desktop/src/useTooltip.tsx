import { useEffect, useId, useState, type SyntheticEvent, type KeyboardEvent } from 'react';
import { createPortal } from 'react-dom';

// Render outside the composer so panel overflow cannot clip the bubble.
export function useTooltip(label: string) {
  const id = useId();
  const [anchor, setAnchor] = useState<DOMRect | null>(null);
  const show = (event: SyntheticEvent<HTMLElement>) => {
    if (!event.currentTarget.closest('details[open]')) setAnchor(event.currentTarget.getBoundingClientRect());
  };
  const hide = () => setAnchor(null);
  useEffect(() => {
    if (!anchor) return;
    window.addEventListener('resize', hide);
    window.addEventListener('scroll', hide, true);
    return () => {
      window.removeEventListener('resize', hide);
      window.removeEventListener('scroll', hide, true);
    };
  }, [anchor]);
  return {
    hide,
    triggerProps: {
      'aria-describedby': anchor ? id : undefined,
      onPointerEnter: show, onPointerLeave: hide, onFocus: show, onBlur: hide,
      onPointerDown: hide, onClick: hide,
      onKeyDown: (event: KeyboardEvent<HTMLElement>) => {
        if (['Escape', 'Enter', ' '].includes(event.key)) hide();
      },
    },
    tooltip: anchor && createPortal(<span id={id} role="tooltip" className="kyro-tooltip" style={{ left: Math.max(72, Math.min(window.innerWidth - 72, anchor.x + anchor.width / 2)), top: anchor.top - 8 }}>{label}</span>, document.body),
  };
}
