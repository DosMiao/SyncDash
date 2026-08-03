import { Sheet } from './Sheet.tsx';

interface ConfirmAction {
  label: string;
  onConfirm: () => void;
  danger?: boolean;
  disabled?: boolean;
}

export function ConfirmDialog({ title, message, action, onCancel }: {
  title: string;
  message: string;
  action: ConfirmAction;
  onCancel: () => void;
}) {
  // A destructive confirmation opens with Cancel focused, so Enter never carries out the
  // destruction the dialog exists to question. A disabled action cannot be the safe default either.
  const dangerConfirmation = action.danger && !action.disabled;
  return (
    <Sheet
      title={title}
      onClose={onCancel}
      footer={
        <>
          <button type="button" className="btn" autoFocus={dangerConfirmation} onClick={onCancel}>Cancel (Esc)</button>
          <button
            type="button"
            className={'btn ' + (action.danger ? 'danger' : 'accent')}
            disabled={action.disabled}
            autoFocus={!dangerConfirmation && !action.disabled}
            onClick={() => { onCancel(); action.onConfirm(); }}
          >
            {action.label}
          </button>
        </>
      }
    >
      <div className="dialog-message">{message}</div>
    </Sheet>
  );
}
