import { ArrowUpRight, Cursor, Sparkle, type Icon } from '@phosphor-icons/react';
import { SpotlightCard } from './SpotlightCard';
import mascot from './assets/kyro-spark.png';

type Suggestion = { label: string; detail: string; icon: Icon; prompt: string };

// Decorative motion stays in CSS: no animation loop or pointer-driven React renders.
export function CreativeWelcome({ suggestions, focusComposer, prepareDraft }: {
  suggestions: Suggestion[];
  focusComposer: () => void;
  prepareDraft: (prompt: string) => void;
}) {
  return <div className="start-screen">
    <div className="idea-stage">
      <div className="start-intro">
        <span className="canvas-label"><Sparkle size={17} weight="fill" /> Votre prochain projet commence ici.</span>
        <h2>Vos idées,<br /><span>en grand.</span></h2>
        <p>Un outil à inventer, un quotidien à simplifier. Tout commence avec vous.</p>
        <button className="start-button" onClick={focusComposer}>Décrire mon idée <ArrowUpRight size={20} /></button>
      </div>
      <div className="idea-art" aria-hidden="true">
        <div className="idea-confetti">{Array.from({ length: 12 }, (_, index) => <i key={index} />)}</div>
        <img className="idea-mascot" src={mascot} width="1280" height="1280" alt="" fetchPriority="high" draggable={false} />
        <span className="idea-spark"><Sparkle size={42} weight="fill" /></span>
      </div>
    </div>
    <div className="starter-list">
      <p>Ou partez d’une première piste</p>
      <div className="starter-options">{suggestions.map(({ label, detail, icon: Icon, prompt }) => <SpotlightCard key={label}>
        <button className="starter-button" onClick={() => prepareDraft(prompt)}>
          <span className="starter-icon"><Icon size={24} weight="duotone" /></span>
          <span className="starter-copy"><strong>{label}</strong><span>{detail}</span></span>
          <ArrowUpRight className="starter-arrow" size={19} />
        </button>
      </SpotlightCard>)}</div>
    </div>
    <div className="canvas-note"><Cursor size={16} /><span>Votre future application prendra place ici.</span></div>
  </div>;
}
