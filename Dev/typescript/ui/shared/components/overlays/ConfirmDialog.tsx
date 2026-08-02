import { Sheet } from './Sheet.tsx';

export interface ConfirmAction {
  label: string;
  onConfirm: () => void;
  danger?: boolean;
  disabled?: boolean;
}

export function ConfirmDialog({ title, message, actions, onCancel }: {
  title: string;
  message: string;
  actions: ConfirmAction[];
  onCancel: () => void;
}) {
  const dangerConfirmation = actions.some((action) => action.danger && !action.disabled);
  const firstEnabledAction = actions.findIndex((action) => !action.disabled);
  return (
    <Sheet
      title={title}
      onClose={onCancel}
      footer={
        <>
          <button type="button" className="btn" autoFocus={dangerConfirmation} onClick={onCancel}>Cancel (Esc)</button>
          {actions.map((action, index) => (
            <button
              key={action.label}
              type="button"
              className={'btn ' + (action.danger ? 'danger' : 'accent')}
              disabled={action.disabled}
              autoFocus={!dangerConfirmation && index === firstEnabledAction}
              onClick={() => { onCancel(); action.onConfirm(); }}
            >
              {action.label}
            </button>
          ))}
        </>
      }
    >
      <div className="dialog-message">{message}</div>
    </Sheet>
  );
}
