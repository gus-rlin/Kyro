import { useEffect, useRef } from 'react';
import { CaretDown, Check, WarningCircle } from '@phosphor-icons/react';

// Reuse Kyro's workspace menu rows, selected state and disclosure interaction.
export function ComposerChoice<T extends string>({ label, selected, choices, onSelect }: {
  label: string; selected: T; choices: readonly { value: T; label: string; description?: string; hint?: string; danger?: boolean; disabled?:boolean }[]; onSelect: (value: T) => void;
}) {
  const menu = useRef<HTMLDetailsElement>(null);
  const active = choices.find((choice) => choice.value === selected)!;
  useEffect(() => {
    const closeOutside = (event: PointerEvent) => {
      if (!menu.current?.contains(event.target as Node)) menu.current?.removeAttribute('open');
    };
    document.addEventListener('pointerdown', closeOutside);
    return () => document.removeEventListener('pointerdown', closeOutside);
  }, []);
  return <details ref={menu} className="composer-choice" name="composer-choice" onKeyDown={(event) => {
    const root = menu.current!;
    const trigger = root.querySelector('summary')!;
    if (event.key === 'Escape' && root.open) {
      root.removeAttribute('open'); trigger.focus(); event.stopPropagation(); event.preventDefault();
    } else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      event.preventDefault(); event.stopPropagation();
      const options = Array.from(root.querySelectorAll<HTMLButtonElement>('button:not(:disabled)'));
      const current = options.indexOf(document.activeElement as HTMLButtonElement);
      root.open = true;
      const index = event.key === 'Home' ? 0 : event.key === 'End' ? options.length - 1 : current < 0 ? 0 : (current + (event.key === 'ArrowDown' ? 1 : -1) + options.length) % options.length;
      options[index]?.focus();
    }
  }}>
    <summary aria-label={`${label} : ${active.label}`}><span>{active.label}</span><CaretDown size={13} /></summary>
    <div className="workspace-options composer-choice-options" role="group" aria-label={`Choix ${label.toLocaleLowerCase('fr')}`}>
      {choices.map((choice) => <button type="button" key={choice.value} disabled={choice.disabled} aria-pressed={choice.value === selected} data-danger={choice.danger || undefined} onClick={() => {
        onSelect(choice.value); menu.current?.removeAttribute('open'); menu.current?.querySelector('summary')?.focus();
      }}>{choice.danger && <WarningCircle size={17} aria-hidden="true" />}<span><strong>{choice.label}</strong>{choice.description && <small title={choice.hint}>{choice.description}</small>}</span>{choice.value === selected && <Check size={17} className="workspace-check" />}</button>)}
    </div>
  </details>;
}
