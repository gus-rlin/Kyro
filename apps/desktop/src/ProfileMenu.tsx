import { useRef, type KeyboardEvent } from 'react';
import { Gear, Gauge, SignOut, UserPlus } from '@phosphor-icons/react';

export function ProfileMenu({ onNotice }: { onNotice: (notice: { title: string; body: string }) => void }) {
  const menu = useRef<HTMLDetailsElement>(null);
  function choose(title: string, body: string) {
    menu.current?.removeAttribute('open');
    menu.current?.querySelector('summary')?.focus();
    onNotice({ title, body });
  }
  function navigate(event: KeyboardEvent<HTMLDetailsElement>) {
    const root = menu.current!;
    const trigger = root.querySelector('summary')!;
    if (event.key === 'Escape') {
      root.removeAttribute('open');
      trigger.focus();
      event.stopPropagation();
    } else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      event.preventDefault();
      root.open = true;
      const buttons = Array.from(root.querySelectorAll<HTMLButtonElement>('button:not(:disabled)'));
      const current = buttons.indexOf(document.activeElement as HTMLButtonElement);
      const index = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : current < 0 ? (event.key === 'ArrowUp' ? buttons.length - 1 : 0) : (current + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length;
      buttons[index]?.focus();
    }
  }
  return <details ref={menu} className="profile" name="application-menu" onKeyDown={navigate}>
    <summary aria-label="Profil"><span className="profile-avatar">K</span></summary>
    <div className="menu-popup profile-popup" role="region" aria-label="Menu du profil">
      <header className="profile-identity"><span className="profile-avatar" aria-hidden="true">K</span><div><strong>Profil</strong><small>Aucun compte connecté</small></div></header>
      <div className="profile-actions">
        <button type="button" onClick={() => choose('Utilisation', 'Connectez un compte pour consulter votre quota et son pourcentage d’utilisation. Aucun quota n’est disponible pour ce profil local.')}><Gauge size={18} aria-hidden="true" /><span>Utilisation</span></button>
        <button type="button" onClick={() => choose('Inviter un ami', 'Les invitations seront disponibles avec un compte connecté. Aucun lien d’invitation n’a été créé.')}><UserPlus size={18} aria-hidden="true" /><span>Inviter un ami</span></button>
        <button type="button" onClick={() => choose('Paramètres', 'Les paramètres du compte ne sont pas encore connectés. Les réglages du message restent disponibles dans le chat.')}><Gear size={18} aria-hidden="true" /><span>Paramètres</span></button>
      </div>
      <hr />
      <button type="button" className="profile-signout" disabled><SignOut size={18} aria-hidden="true" /><span>Se déconnecter</span></button>
    </div>
  </details>;
}
