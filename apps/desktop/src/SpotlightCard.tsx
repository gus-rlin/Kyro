import { useRef, type PropsWithChildren, type PointerEvent } from 'react';

// Adapted from React Bits SpotlightCard. License: ../THIRD_PARTY_NOTICES.md.
// Direct CSS updates keep pointer movement outside React's render cycle.
export function SpotlightCard({ children }: PropsWithChildren) {
  const card = useRef<HTMLDivElement>(null);
  function move(event: PointerEvent<HTMLDivElement>) {
    if (!card.current || event.pointerType === 'touch' || window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    const bounds = card.current.getBoundingClientRect();
    card.current.style.setProperty('--mouse-x', `${event.clientX - bounds.left}px`);
    card.current.style.setProperty('--mouse-y', `${event.clientY - bounds.top}px`);
  }
  return <div ref={card} className="spotlight-card" onPointerMove={move}>{children}</div>;
}
