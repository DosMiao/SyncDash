export type InteractionCommand =
  | 'dismiss'
  | 'find'
  | 'compare'
  | 'synchronize'
  | 'zoom_in'
  | 'zoom_out'
  | 'zoom_reset';

export type InteractionLayerKind =
  | 'application'
  | 'workspace'
  | 'auxiliary_panel'
  | 'popover'
  | 'menu'
  | 'modal';

export interface InteractionKeyEvent {
  key: string;
  altKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  repeat: boolean;
  isComposing: boolean;
}

export interface InteractionLayerEntry {
  id: symbol;
  parentId: symbol | null;
  kind: InteractionLayerKind;
  order: number;
  handler(command: InteractionCommand): (() => void) | undefined;
  root: HTMLElement | null;
}

export type InteractionCommandResolution =
  | { disposition: 'unhandled' }
  | { disposition: 'blocked'; layerId: symbol }
  | { disposition: 'handled'; layerId: symbol; run: () => void };

const ROOT_LAYER_PRIORITY: Readonly<Record<InteractionLayerKind, number>> = {
  application: 0,
  workspace: 10,
  auxiliary_panel: 20,
  popover: 30,
  menu: 30,
  modal: 40,
};

function capturesUnhandledCommands(kind: InteractionLayerKind): boolean {
  return kind === 'auxiliary_panel' || kind === 'popover' || kind === 'menu' || kind === 'modal';
}

function isGlobalAccessibilityCommand(command: InteractionCommand): boolean {
  return command === 'zoom_in' || command === 'zoom_out' || command === 'zoom_reset';
}

function layerStackingPath(
  layer: InteractionLayerEntry,
  layersById: ReadonlyMap<symbol, InteractionLayerEntry>,
  visiting: Set<symbol> = new Set(),
): number[] {
  if (visiting.has(layer.id)) throw new Error('Interaction layer parent cycle');
  visiting.add(layer.id);
  const parent = layer.parentId === null ? undefined : layersById.get(layer.parentId);
  const path = parent
    ? [...layerStackingPath(parent, layersById, visiting), ROOT_LAYER_PRIORITY[layer.kind], layer.order]
    : [ROOT_LAYER_PRIORITY[layer.kind], layer.order];
  visiting.delete(layer.id);
  return path;
}

function compareStackingPaths(left: readonly number[], right: readonly number[]): number {
  const sharedLength = Math.min(left.length, right.length);
  for (let index = 0; index < sharedLength; index += 1) {
    const difference = (right[index] ?? 0) - (left[index] ?? 0);
    if (difference !== 0) return difference;
  }
  return right.length - left.length;
}

export function interactionCommandFromKey(event: InteractionKeyEvent): InteractionCommand | null {
  if (event.repeat || event.isComposing || event.altKey) return null;
  const primaryModifier = event.ctrlKey || event.metaKey;
  const normalizedKey = event.key.toLowerCase();

  if (!primaryModifier && !event.shiftKey && event.key === 'Escape') return 'dismiss';
  if (primaryModifier && !event.shiftKey && normalizedKey === 'f') return 'find';
  if (!primaryModifier && !event.shiftKey && event.key === 'F5') return 'compare';
  if (!primaryModifier && !event.shiftKey && event.key === 'F9') return 'synchronize';
  if (primaryModifier && !event.shiftKey && normalizedKey === 'r') return 'compare';
  if (primaryModifier && !event.shiftKey && (event.key === '+' || event.key === '=')) return 'zoom_in';
  if (primaryModifier && !event.shiftKey && (event.key === '-' || event.key === '_')) return 'zoom_out';
  if (primaryModifier && !event.shiftKey && event.key === '0') return 'zoom_reset';
  return null;
}

export function orderedInteractionLayers(
  layers: readonly InteractionLayerEntry[],
): InteractionLayerEntry[] {
  const layersById = new Map(layers.map((layer) => [layer.id, layer]));
  return [...layers].sort((left, right) => compareStackingPaths(
    layerStackingPath(left, layersById),
    layerStackingPath(right, layersById),
  ));
}

export function resolveInteractionCommand(
  command: InteractionCommand,
  layers: readonly InteractionLayerEntry[],
): InteractionCommandResolution {
  for (const layer of orderedInteractionLayers(layers)) {
    const run = layer.handler(command);
    if (run) return { disposition: 'handled', layerId: layer.id, run };
    if (!isGlobalAccessibilityCommand(command) && capturesUnhandledCommands(layer.kind)) {
      return { disposition: 'blocked', layerId: layer.id };
    }
  }
  return { disposition: 'unhandled' };
}
