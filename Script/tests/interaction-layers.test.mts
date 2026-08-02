import assert from 'node:assert/strict';
import test from 'node:test';

import {
  interactionCommandFromKey,
  orderedInteractionLayers,
  resolveInteractionCommand,
  type InteractionCommand,
  type InteractionLayerEntry,
  type InteractionLayerKind,
} from '#ui/shared/interaction/interactionLayers.ts';

function key(
  value: string,
  overrides: Partial<Parameters<typeof interactionCommandFromKey>[0]> = {},
) {
  return {
    key: value,
    altKey: false,
    ctrlKey: false,
    metaKey: false,
    shiftKey: false,
    repeat: false,
    isComposing: false,
    ...overrides,
  };
}

function layer(
  kind: InteractionLayerKind,
  order: number,
  handlers: Partial<Record<InteractionCommand, () => void>> = {},
  parentId: symbol | null = null,
): InteractionLayerEntry {
  return {
    id: Symbol(`${kind}-${order}`),
    parentId,
    kind,
    order,
    root: null,
    handler: (command) => handlers[command],
  };
}

test('keyboard grammar maps only exact, non-repeating application commands', () => {
  assert.equal(interactionCommandFromKey(key('Escape')), 'dismiss');
  assert.equal(interactionCommandFromKey(key('F5')), 'compare');
  assert.equal(interactionCommandFromKey(key('F9')), 'synchronize');
  assert.equal(interactionCommandFromKey(key('r', { ctrlKey: true })), 'compare');
  assert.equal(interactionCommandFromKey(key('f', { metaKey: true })), 'find');
  assert.equal(interactionCommandFromKey(key('=', { ctrlKey: true })), 'zoom_in');
  assert.equal(interactionCommandFromKey(key('-', { metaKey: true })), 'zoom_out');
  assert.equal(interactionCommandFromKey(key('0', { ctrlKey: true })), 'zoom_reset');
  assert.equal(interactionCommandFromKey(key('Escape', { repeat: true })), null);
  assert.equal(interactionCommandFromKey(key('F5', { isComposing: true })), null);
  assert.equal(interactionCommandFromKey(key('F5', { altKey: true })), null);
  assert.equal(interactionCommandFromKey(key('r', { ctrlKey: true, shiftKey: true })), null);
});

test('an exclusive top layer blocks commands from every underlying surface', () => {
  const application = layer('application', 1, { compare: () => {} });
  const workspace = layer('workspace', 2, { find: () => {} });
  const logPanel = layer('auxiliary_panel', 3, { find: () => {} });
  const popover = layer('popover', 4, { dismiss: () => {} });

  const dismiss = resolveInteractionCommand('dismiss', [application, workspace, logPanel, popover]);
  assert.ok(dismiss.disposition !== 'unhandled');
  assert.equal(dismiss.layerId, popover.id);
  assert.deepEqual(resolveInteractionCommand('find', [application, workspace, logPanel, popover]), {
    disposition: 'blocked',
    layerId: popover.id,
  });
  assert.deepEqual(resolveInteractionCommand('compare', [application, workspace, logPanel, popover]), {
    disposition: 'blocked',
    layerId: popover.id,
  });
});

test('log search owns find and blocks run commands while the log is active', () => {
  const application = layer('application', 1, { compare: () => {} });
  const workspace = layer('workspace', 2, { find: () => {} });
  const logPanel = layer('auxiliary_panel', 3, { find: () => {} });

  const find = resolveInteractionCommand('find', [application, workspace, logPanel]);
  assert.ok(find.disposition !== 'unhandled');
  assert.equal(find.layerId, logPanel.id);
  assert.deepEqual(resolveInteractionCommand('compare', [application, workspace, logPanel]), {
    disposition: 'blocked',
    layerId: logPanel.id,
  });
});

test('a child layer stays above its parent without outranking a newer root modal', () => {
  const firstModal = layer('modal', 10, { dismiss: () => {} });
  const nestedMenu = layer('menu', 11, { dismiss: () => {} }, firstModal.id);
  const secondModal = layer('modal', 12, { dismiss: () => {} });

  assert.equal(orderedInteractionLayers([firstModal, nestedMenu])[0]?.id, nestedMenu.id);
  assert.equal(orderedInteractionLayers([firstModal, nestedMenu, secondModal])[0]?.id, secondModal.id);
});

test('zoom remains globally accessible across an exclusive layer', () => {
  const application = layer('application', 1, { zoom_in: () => {} });
  const modal = layer('modal', 2, { dismiss: () => {} });
  const zoomIn = resolveInteractionCommand('zoom_in', [application, modal]);
  assert.ok(zoomIn.disposition !== 'unhandled');
  assert.equal(zoomIn.layerId, application.id);
});
