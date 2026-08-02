import type { ReactNode } from 'react';

export function Placeholder({ icon, title, description }: {
  icon: ReactNode;
  title: string;
  description?: string;
}) {
  return (
    <div className="placeholder">
      <div className="placeholder-icon">{icon}</div>
      <div className="placeholder-title">{title}</div>
      {description ? <div className="placeholder-desc">{description}</div> : null}
    </div>
  );
}
